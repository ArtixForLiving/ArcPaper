//! Presenter module managing Direct3D 11 device, DXGI swapchain, and presentation to the WorkerW window.

use windows::Win32::Foundation::{E_FAIL, HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Error, Interface, Result};

/// Manages Direct3D 11 device contexts and presentation to the desktop wallpaper window.
pub struct Presenter {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub swap_chain: IDXGISwapChain1,
    pub render_target_view: Option<ID3D11RenderTargetView>,
    pub child_hwnd: Option<HWND>,
    pub width: u32,
    pub height: u32,
}

impl Drop for Presenter {
    fn drop(&mut self) {
        if let Some(child) = self.child_hwnd.take() {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
                let _ = DestroyWindow(child);
            }
        }
    }
}

unsafe extern "system" fn child_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    match msg {
        // Explicitly return HTTRANSPARENT (-1) for hit-testing so all mouse clicks,
        // double-clicks, and drag selections pass straight through to SHELLDLL_DefView.
        windows::Win32::UI::WindowsAndMessaging::WM_NCHITTEST => {
            windows::Win32::Foundation::LRESULT(-1)
        }
        // Hand WM_SETCURSOR over to DefWindowProcW to prevent cursor freezing on the loading ring
        windows::Win32::UI::WindowsAndMessaging::WM_SETCURSOR => unsafe {
            windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
        },
        _ => unsafe {
            windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
        },
    }
}

static REGISTER_CLASS_ONCE: std::sync::Once = std::sync::Once::new();

fn register_child_host_class() -> Result<()> {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{PCWSTR, w};

    let mut reg_result = Ok(());
    REGISTER_CLASS_ONCE.call_once(|| {
        let hinstance = match unsafe { GetModuleHandleW(PCWSTR::null()) } {
            Ok(h) => h,
            Err(e) => {
                reg_result = Err(e);
                return;
            }
        };
        let hinst = HINSTANCE(hinstance.0);
        let class_name = w!("ArcPaperChildHost");

        let wcx = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(child_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: HICON::default(),
            hCursor: HCURSOR::default(),
            hbrBackground: Default::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: HICON::default(),
        };

        unsafe {
            let _ = RegisterClassExW(&wcx);
        }
    });

    reg_result
}

fn create_child_host_hwnd(parent: HWND, x: i32, y: i32, width: i32, height: i32) -> Result<HWND> {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{PCWSTR, w};

    register_child_host_class()?;

    let hinstance = unsafe { GetModuleHandleW(PCWSTR::null())? };
    let hinst = HINSTANCE(hinstance.0);
    let class_name = w!("ArcPaperChildHost");

    let actual_w = width.max(1);
    let actual_h = height.max(1);

    unsafe {
        // 1. Create own top-level popup window with mouse pass-through extended styles
        let ex_style = WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
        let hwnd = CreateWindowExW(
            ex_style,
            class_name,
            w!("ArcPaperSurface"),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            actual_w,
            actual_h,
            None, // No initial cross-process parent to avoid ERROR_INVALID_PARAMETER
            None,
            Some(hinst),
            None,
        )?;

        // 2. Reparent to WorkerW and convert style to WS_CHILD
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let child_style = (style & !WS_POPUP.0) | WS_CHILD.0;
        let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, child_style as isize);

        let curr_ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let _ = SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            (curr_ex_style | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as isize,
        );

        if !parent.0.is_null() {
            let _ = SetParent(hwnd, Some(parent));
        }

        // 3. Ensure exact position and visibility inside WorkerW
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            actual_w,
            actual_h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );

        Ok(hwnd)
    }
}

/// Internal helper to extract the backbuffer texture and instantiate its Render Target View.
fn create_rtv_for_swapchain(
    device: &ID3D11Device,
    swap_chain: &IDXGISwapChain1,
) -> Result<ID3D11RenderTargetView> {
    let back_buffer: ID3D11Texture2D = unsafe { swap_chain.GetBuffer(0)? };
    let mut rtv = None;
    unsafe {
        device.CreateRenderTargetView(&back_buffer, None, Some(&mut rtv))?;
    }
    rtv.ok_or_else(|| Error::from_hresult(E_FAIL))
}

impl Presenter {
    /// Creates a dedicated child render window hosted inside the parent desktop HWND.
    /// This ensures our process owns the target HWND, avoiding cross-process E_ACCESSDENIED errors.
    fn create_child_render_window(parent: HWND, width: u32, height: u32) -> Result<HWND> {
        create_child_host_hwnd(parent, 0, 0, width as i32, height as i32)
    }

    /// Initializes a new Direct3D 11 device and binds a flip-model swapchain to the target window.
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let (target_hwnd, child_hwnd) = if !hwnd.0.is_null() {
            match Self::create_child_render_window(hwnd, width, height) {
                Ok(child) => (child, Some(child)),
                Err(_) => (hwnd, None),
            }
        } else {
            (hwnd, None)
        };

        let mut device = None;
        let mut context = None;

        let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_10_0,
        ];

        // 1. Create Direct3D 11 hardware device with video decoding and BGRA support
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }

        let device = device.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        let context = context.ok_or_else(|| Error::from_hresult(E_FAIL))?;

        // 2. Enable multithread protection on the device for safe concurrent decoding/rendering
        if let Ok(multithread) = device.cast::<ID3D10Multithread>() {
            unsafe {
                let _ = multithread.SetMultithreadProtected(true);
            }
        }

        // 3. Query DXGI Factory from D3D11 device
        let dxgi_device: IDXGIDevice2 = device.cast()?;
        let adapter: IDXGIAdapter = unsafe { dxgi_device.GetAdapter()? };
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent()? };

        // 4. Create flip-model SwapChain attached to the WorkerW window
        let swap_chain_desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
            Flags: 0,
        };

        let swap_chain = unsafe {
            factory.CreateSwapChainForHwnd(&device, target_hwnd, &swap_chain_desc, None, None)?
        };

        // 5. Create Render Target View from backbuffer 0
        let rtv = Self::create_render_target_view(&device, &swap_chain)?;

        Ok(Self {
            device,
            context,
            swap_chain,
            render_target_view: Some(rtv),
            child_hwnd,
            width,
            height,
        })
    }

    /// Internal helper to extract the backbuffer texture and instantiate its Render Target View.
    fn create_render_target_view(
        device: &ID3D11Device,
        swap_chain: &IDXGISwapChain1,
    ) -> Result<ID3D11RenderTargetView> {
        create_rtv_for_swapchain(device, swap_chain)
    }

    /// Resizes the SwapChain buffers when display resolution or window size changes.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        // Resize child window if one was created
        if let Some(child) = self.child_hwnd {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
                };
                let _ = SetWindowPos(
                    child,
                    None,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }

        // Mandatory DirectX requirement: drop existing RTV references before calling ResizeBuffers
        self.render_target_view = None;

        unsafe {
            self.swap_chain.ResizeBuffers(
                2,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }

        // Recreate the Render Target View for the resized backbuffer
        self.render_target_view = Some(Self::create_render_target_view(
            &self.device,
            &self.swap_chain,
        )?);
        self.width = width;
        self.height = height;

        Ok(())
    }

    /// Presents the rendered backbuffer to the display.
    pub fn present(&self, vsync: bool) -> Result<()> {
        let sync_interval = if vsync { 1 } else { 0 };
        unsafe { self.swap_chain.Present(sync_interval, DXGI_PRESENT(0)).ok() }
    }

    /// Flushes the pipeline and signals the GPU driver to trim internal allocation caches during hibernation.
    pub fn trim_gpu(&self) {
        unsafe {
            self.context.ClearState();
            self.context.Flush();
            if let Ok(dxgi_device3) = self.device.cast::<IDXGIDevice3>() {
                dxgi_device3.Trim();
            }
        }
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    pub fn swap_chain(&self) -> &IDXGISwapChain1 {
        &self.swap_chain
    }

    pub fn render_target_view(&self) -> Option<&ID3D11RenderTargetView> {
        self.render_target_view.as_ref()
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Queries the underlying device removal status (`S_OK` when healthy).
    pub fn device_removed_reason(&self) -> windows::core::HRESULT {
        unsafe {
            self.device
                .GetDeviceRemovedReason()
                .map(|_| windows::Win32::Foundation::S_OK)
                .unwrap_or_else(|e| e.code())
        }
    }
}

/// Main-thread owned child window hosted inside the desktop WorkerW.
/// Uses `WM_NCHITTEST -> HTTRANSPARENT (-1)` and `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`
/// for seamless mouse click and selection pass-through to the Windows desktop icons.
pub struct ChildHostWindow {
    hwnd: HWND,
}

impl ChildHostWindow {
    /// Creates and configures a child host window inside `parent_workerw`.
    pub fn create(parent_workerw: HWND, x: i32, y: i32, width: i32, height: i32) -> Result<Self> {
        let hwnd = create_child_host_hwnd(parent_workerw, x, y, width, height)?;
        Ok(Self { hwnd })
    }

    /// Returns the window handle.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Repositions and resizes the child window.
    pub fn reposition(&self, x: i32, y: i32, width: i32, height: i32) {
        if !self.hwnd.0.is_null() {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
                };
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    x,
                    y,
                    width.max(1),
                    height.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Reparents the child window to a new WorkerW parent and updates its position.
    pub fn reparent(&self, new_workerw: HWND, x: i32, y: i32, width: i32, height: i32) {
        if !self.hwnd.0.is_null() {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::SetParent;
                let _ = SetParent(self.hwnd, Some(new_workerw));
            }
            self.reposition(x, y, width, height);
        }
    }
}

impl Drop for ChildHostWindow {
    fn drop(&mut self) {
        if !self.hwnd.0.is_null() {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Shared Direct3D 11 hardware device, synchronized context, and DXGI factory.
pub struct GraphicsDevice {
    pub device: ID3D11Device,
    context: std::sync::Mutex<ID3D11DeviceContext>,
    pub factory: IDXGIFactory2,
}

// Safety: ID3D11Device has SetMultithreadProtected(true) enabled,
// context access is synchronized via std::sync::Mutex,
// and IDXGIFactory2 methods called are thread-safe.
unsafe impl Send for GraphicsDevice {}
unsafe impl Sync for GraphicsDevice {}

impl GraphicsDevice {
    /// Creates a hardware D3D11 device with BGRA and video decoding support, enables multithreading protection,
    /// and extracts the DXGI factory.
    pub fn new() -> Result<Self> {
        let mut device = None;
        let mut context = None;

        let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_10_0,
        ];

        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }

        let device = device.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        let context = context.ok_or_else(|| Error::from_hresult(E_FAIL))?;

        if let Ok(multithread) = device.cast::<ID3D10Multithread>() {
            unsafe {
                let _ = multithread.SetMultithreadProtected(true);
            }
        }

        let dxgi_device: IDXGIDevice2 = device.cast()?;
        let adapter: IDXGIAdapter = unsafe { dxgi_device.GetAdapter()? };
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent()? };

        Ok(Self {
            device,
            context: std::sync::Mutex::new(context),
            factory,
        })
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn factory(&self) -> &IDXGIFactory2 {
        &self.factory
    }

    pub fn with_context<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&ID3D11DeviceContext) -> R,
    {
        let ctx = self
            .context
            .lock()
            .expect("GraphicsDevice context mutex poisoned");
        f(&ctx)
    }

    pub fn trim_gpu(&self) {
        self.with_context(|ctx| unsafe {
            ctx.ClearState();
            ctx.Flush();
        });
        if let Ok(dxgi_device3) = self.device.cast::<IDXGIDevice3>() {
            unsafe {
                dxgi_device3.Trim();
            }
        }
    }

    pub fn device_removed_reason(&self) -> windows::core::HRESULT {
        unsafe {
            self.device
                .GetDeviceRemovedReason()
                .map(|_| windows::Win32::Foundation::S_OK)
                .unwrap_or_else(|e| e.code())
        }
    }
}

/// Dedicated DXGI flip swapchain and render target view for a single monitor surface.
pub struct MonitorSwapChain {
    swap_chain: IDXGISwapChain1,
    render_target_view: Option<ID3D11RenderTargetView>,
    width: u32,
    height: u32,
}

impl MonitorSwapChain {
    /// Creates a flip-model swapchain bound to `target_hwnd` using `gfx`'s device and factory.
    pub fn new(gfx: &GraphicsDevice, target_hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let actual_w = width.max(1);
        let actual_h = height.max(1);

        let swap_chain_desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: actual_w,
            Height: actual_h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
            Flags: 0,
        };

        let swap_chain = unsafe {
            gfx.factory.CreateSwapChainForHwnd(
                &gfx.device,
                target_hwnd,
                &swap_chain_desc,
                None,
                None,
            )?
        };

        // Best-effort: disable Alt+Enter association
        unsafe {
            let _ = gfx
                .factory
                .MakeWindowAssociation(target_hwnd, DXGI_MWA_NO_ALT_ENTER);
        }

        let rtv = create_rtv_for_swapchain(&gfx.device, &swap_chain)?;

        Ok(Self {
            swap_chain,
            render_target_view: Some(rtv),
            width: actual_w,
            height: actual_h,
        })
    }

    /// Resizes the SwapChain buffers when display resolution changes.
    pub fn resize(&mut self, device: &ID3D11Device, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        // Mandatory DirectX requirement: drop existing RTV references before calling ResizeBuffers
        self.render_target_view = None;

        unsafe {
            self.swap_chain.ResizeBuffers(
                2,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }

        self.render_target_view = Some(create_rtv_for_swapchain(device, &self.swap_chain)?);
        self.width = width;
        self.height = height;

        Ok(())
    }

    /// Presents the rendered backbuffer to the display.
    /// Returns `Ok(true)` when presented, `Ok(false)` when `DXGI_ERROR_WAS_STILL_DRAWING` skipped frame non-blockingly.
    pub fn present(&self, vsync: bool, do_not_wait: bool) -> Result<bool> {
        let sync_interval = if vsync { 1 } else { 0 };
        let mut flags = DXGI_PRESENT(0);
        if do_not_wait {
            flags |= DXGI_PRESENT_DO_NOT_WAIT;
        }

        let res = unsafe { self.swap_chain.Present(sync_interval, flags).ok() };
        match res {
            Ok(_) => Ok(true),
            Err(err) if is_was_still_drawing_error(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub fn render_target_view(&self) -> Option<&ID3D11RenderTargetView> {
        self.render_target_view.as_ref()
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

/// Returns `true` when `hr` is `DXGI_ERROR_WAS_STILL_DRAWING` indicating GPU presentation is pending.
pub fn is_was_still_drawing_hresult(hr: windows::core::HRESULT) -> bool {
    hr == DXGI_ERROR_WAS_STILL_DRAWING
}

/// Returns `true` when `err` represents `DXGI_ERROR_WAS_STILL_DRAWING`.
pub fn is_was_still_drawing_error(err: &Error) -> bool {
    is_was_still_drawing_hresult(err.code())
}

/// Returns `true` when `hr` represents a recoverable DXGI GPU device loss or reset.
pub fn is_device_lost_hresult(hr: windows::core::HRESULT) -> bool {
    hr == DXGI_ERROR_DEVICE_REMOVED
        || hr == DXGI_ERROR_DEVICE_RESET
        || hr == DXGI_ERROR_DEVICE_HUNG
        || hr == DXGI_ERROR_DRIVER_INTERNAL_ERROR
}

/// Returns `true` when `err` represents a DXGI device removal or reset error.
pub fn is_device_lost_error(err: &Error) -> bool {
    is_device_lost_hresult(err.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_device_lost_hresult_classification() {
        assert!(is_device_lost_hresult(DXGI_ERROR_DEVICE_REMOVED));
        assert!(is_device_lost_hresult(DXGI_ERROR_DEVICE_RESET));
        assert!(is_device_lost_hresult(DXGI_ERROR_DEVICE_HUNG));
        assert!(is_device_lost_hresult(DXGI_ERROR_DRIVER_INTERNAL_ERROR));
        assert!(!is_device_lost_hresult(E_FAIL));
        assert!(!is_device_lost_hresult(windows::Win32::Foundation::S_OK));
    }

    #[test]
    fn test_is_was_still_drawing_classification() {
        assert!(is_was_still_drawing_hresult(DXGI_ERROR_WAS_STILL_DRAWING));
        assert!(!is_device_lost_hresult(DXGI_ERROR_WAS_STILL_DRAWING));
        assert!(!is_was_still_drawing_hresult(DXGI_ERROR_DEVICE_REMOVED));
        assert!(!is_was_still_drawing_hresult(
            windows::Win32::Foundation::S_OK
        ));

        let err = Error::from_hresult(DXGI_ERROR_WAS_STILL_DRAWING);
        assert!(is_was_still_drawing_error(&err));
        let rem_err = Error::from_hresult(DXGI_ERROR_DEVICE_REMOVED);
        assert!(!is_was_still_drawing_error(&rem_err));
    }

    #[test]
    fn test_graphics_device_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<GraphicsDevice>();
        assert_sync::<GraphicsDevice>();
    }

    #[test]
    fn test_child_host_window_lifecycle() {
        if let Ok(wnd) = ChildHostWindow::create(HWND::default(), 10, 20, 300, 200) {
            assert!(!wnd.hwnd().0.is_null());
            wnd.reposition(15, 25, 400, 250);
            wnd.reparent(HWND::default(), 0, 0, 500, 300);
        }
    }

    #[test]
    fn test_monitor_swapchain_creation_and_resize() {
        if let (Ok(gfx), Ok(wnd)) = (
            GraphicsDevice::new(),
            ChildHostWindow::create(HWND::default(), 0, 0, 100, 100),
        ) {
            let sc = MonitorSwapChain::new(&gfx, wnd.hwnd(), 100, 100);
            if let Ok(mut swap_chain) = sc {
                assert_eq!(swap_chain.width(), 100);
                assert_eq!(swap_chain.height(), 100);
                assert!(swap_chain.render_target_view().is_some());
                assert!(swap_chain.resize(gfx.device(), 200, 150).is_ok());
                assert_eq!(swap_chain.width(), 200);
                assert_eq!(swap_chain.height(), 150);
                assert!(swap_chain.render_target_view().is_some());
                gfx.trim_gpu();
                assert_eq!(
                    gfx.device_removed_reason(),
                    windows::Win32::Foundation::S_OK
                );
            }
        }
    }
}
