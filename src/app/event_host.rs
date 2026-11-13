//! Hidden Win32 event window (`ArcPaperEventHost`) capturing OS-level broadcasts.
//!
//! Listens for display topology changes (`WM_DISPLAYCHANGE`, `WM_DPICHANGED`,
//! `WM_SETTINGCHANGE`) and Explorer desktop restarts (`TaskbarCreated`), forwarding
//! normalized `HostEvent` occurrences to the registered callback.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEvent {
    DisplayTopologyDirty,
    ExplorerRestarted,
}

#[cfg(windows)]
mod imp {
    use super::HostEvent;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA;

    struct EventHostContext {
        on_event: Box<dyn Fn(HostEvent) + Send + 'static>,
        taskbar_created_msg: u32,
    }

    pub struct ArcPaperEventHost {
        hwnd: HWND,
        ctx: *mut EventHostContext,
    }

    unsafe extern "system" fn event_host_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::{
            DefWindowProcW, EndMenu, SetWindowLongPtrW, WM_CREATE, WM_DISPLAYCHANGE, WM_DPICHANGED,
            WM_SETTINGCHANGE,
        };

        unsafe {
            if msg == WM_CREATE {
                let create_struct =
                    &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
                let ctx_ptr = create_struct.lpCreateParams as isize;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx_ptr);
                return LRESULT(0);
            }

            let ctx_ptr =
                windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ctx_ptr != 0 {
                let ctx = &*(ctx_ptr as *const EventHostContext);

                const SPI_SETWORKAREA: usize = 0x002F;
                const SPI_SETLOGICALDPIOVERRIDE: usize = 0x009F;

                if msg == WM_DISPLAYCHANGE || msg == WM_DPICHANGED {
                    let _ = EndMenu();
                    (ctx.on_event)(HostEvent::DisplayTopologyDirty);
                    return LRESULT(0);
                } else if msg == WM_SETTINGCHANGE {
                    if wparam.0 == SPI_SETWORKAREA || wparam.0 == SPI_SETLOGICALDPIOVERRIDE {
                        (ctx.on_event)(HostEvent::DisplayTopologyDirty);
                        return LRESULT(0);
                    }
                } else if ctx.taskbar_created_msg != 0 && msg == ctx.taskbar_created_msg {
                    (ctx.on_event)(HostEvent::ExplorerRestarted);
                    return LRESULT(0);
                }
            }

            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }

    impl ArcPaperEventHost {
        pub fn new<F>(on_event: F) -> Result<Self, String>
        where
            F: Fn(HostEvent) + Send + 'static,
        {
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::UI::WindowsAndMessaging::{
                CS_HREDRAW, CS_VREDRAW, CreateWindowExW, HCURSOR, HICON, RegisterClassExW,
                RegisterWindowMessageW, SetWindowLongPtrW, WNDCLASSEXW, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_POPUP,
            };
            use windows::core::{PCWSTR, w};

            unsafe {
                let hinstance = match GetModuleHandleW(None) {
                    Ok(h) => h,
                    Err(e) => return Err(format!("GetModuleHandleW failed: {e}")),
                };
                let hinst = HINSTANCE(hinstance.0);
                let class_name = w!("ArcPaperEventHost");

                let wcx = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(event_host_wnd_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinst,
                    hIcon: HICON::default(),
                    hCursor: HCURSOR::default(),
                    hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
                    lpszMenuName: PCWSTR::null(),
                    lpszClassName: class_name,
                    hIconSm: HICON::default(),
                };
                let _ = RegisterClassExW(&wcx);

                let taskbar_created_msg = RegisterWindowMessageW(w!("TaskbarCreated"));

                let ctx = Box::into_raw(Box::new(EventHostContext {
                    on_event: Box::new(on_event),
                    taskbar_created_msg,
                }));

                let hwnd = match CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class_name,
                    w!("ArcPaperEventHostWindow"),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(hinst),
                    Some(ctx as *const _ as *const std::ffi::c_void),
                ) {
                    Ok(h) => h,
                    Err(e) => {
                        drop(Box::from_raw(ctx));
                        return Err(format!("CreateWindowExW failed: {e}"));
                    }
                };

                SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);

                Ok(Self { hwnd, ctx })
            }
        }

        /// Returns the underlying window handle.
        pub fn hwnd(&self) -> HWND {
            self.hwnd
        }
    }

    impl Drop for ArcPaperEventHost {
        fn drop(&mut self) {
            unsafe {
                if !self.hwnd.0.is_null() {
                    use windows::Win32::UI::WindowsAndMessaging::{
                        DestroyWindow, SetWindowLongPtrW,
                    };
                    SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                    let _ = DestroyWindow(self.hwnd);
                    self.hwnd = HWND::default();
                }
                if !self.ctx.is_null() {
                    drop(Box::from_raw(self.ctx));
                    self.ctx = std::ptr::null_mut();
                }
            }
        }
    }
}

#[cfg(windows)]
pub use imp::ArcPaperEventHost;

#[cfg(not(windows))]
pub struct ArcPaperEventHost;

#[cfg(not(windows))]
impl ArcPaperEventHost {
    pub fn new<F>(_on_event: F) -> Result<Self, String>
    where
        F: Fn(HostEvent) + Send + 'static,
    {
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_event_equality_and_debug() {
        let e1 = HostEvent::DisplayTopologyDirty;
        let e2 = HostEvent::DisplayTopologyDirty;
        let e3 = HostEvent::ExplorerRestarted;

        assert_eq!(e1, e2);
        assert_ne!(e1, e3);
        assert_eq!(format!("{:?}", e1), "DisplayTopologyDirty");
        assert_eq!(format!("{:?}", e3), "ExplorerRestarted");

        let cloned = e1;
        assert_eq!(cloned, HostEvent::DisplayTopologyDirty);
    }

    #[cfg(windows)]
    #[test]
    fn test_event_host_creation_and_drop_lifecycle() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let host = ArcPaperEventHost::new(move |_event| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        assert!(host.is_ok(), "Event host should initialize successfully");
        let host = host.unwrap();
        assert!(!host.hwnd().0.is_null());

        drop(host);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
}
