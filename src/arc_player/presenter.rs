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
        windows::Win32::UI::WindowsAndMessaging::WM_SETCURSOR => {
            unsafe { windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl Presenter {
    /// Creates a dedicated child render window hosted inside the parent desktop HWND.
    /// This ensures our process owns the target HWND, avoiding cross-process E_ACCESSDENIED errors.
    fn create_child_render_window(parent: HWND, width: u32, height: u32) -> Result<HWND> {
        use windows::Win32::Foundation::HINSTANCE;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows::core::{PCWSTR, w};

        let hinstance = unsafe { GetModuleHandleW(PCWSTR::null())? };
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

            // 1. Create own top-level popup window with mouse pass-through extended styles
            let ex_style = WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
            let hwnd = CreateWindowExW(
                ex_style,
                class_name,
                w!("ArcPaperSurface"),
                WS_POPUP | WS_VISIBLE,
                0,
                0,
                width as i32,
                height as i32,
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

            let _ = SetParent(hwnd, Some(parent));

            // 3. Ensure exact position and visibility inside WorkerW
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                width as i32,
                height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );

            Ok(hwnd)
        }
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
        let back_buffer: ID3D11Texture2D = unsafe { swap_chain.GetBuffer(0)? };
        let mut rtv = None;
        unsafe {
            device.CreateRenderTargetView(&back_buffer, None, Some(&mut rtv))?;
        }
        rtv.ok_or_else(|| Error::from_hresult(E_FAIL))
    }

    /// Resizes the SwapChain buffers when display resolution or window size changes.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        // Resize child window if one was created
        if let Some(child) = self.child_hwnd {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos};
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
        unsafe {
            self.swap_chain
                .Present(sync_interval, DXGI_PRESENT(0))
                .ok()
        }
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
}
