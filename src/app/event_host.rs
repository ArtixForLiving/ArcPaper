//! Hidden Win32 event window (`ArcPaperEventHost`) capturing OS-level broadcasts.
//!
//! Listens for:
//! - Display topology changes (`WM_DISPLAYCHANGE`, `WM_DPICHANGED`, `WM_SETTINGCHANGE`)
//! - Explorer desktop restarts (`TaskbarCreated`)
//! - Session lock/unlock and Remote Desktop connection changes (`WM_WTSSESSION_CHANGE`)
//! - AC/DC power source, battery saver, battery percent, console display sleep, and ACPI suspend/resume (`WM_POWERBROADCAST`)
//!
//! Normalizes occurrences and forwards [`HostEvent`] instances to the registered callback.

use crate::app::PauseReason;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEvent {
    DisplayTopologyDirty,
    ExplorerRestarted,
    GlobalPauseReasonsChanged(PauseReason),
    ResumeFromSuspend,
}

#[cfg(windows)]
mod imp {
    use super::HostEvent;
    use crate::app::PauseReason;
    use crate::power::battery::{BatteryPolicy, evaluate_battery_pause};
    use crate::power::session::evaluate_session_pause;
    use crate::power::system_probe::{PowerStatus, SystemProbe, WindowsSystemProbe};
    use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Power::{
        HPOWERNOTIFY, RegisterPowerSettingNotification, UnregisterPowerSettingNotification,
    };
    use windows::Win32::System::RemoteDesktop::{
        NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DEVICE_NOTIFY_WINDOW_HANDLE, GWLP_USERDATA, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND,
        PBT_POWERSETTINGCHANGE, WM_CREATE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_POWERBROADCAST,
        WM_SETTINGCHANGE, WM_WTSSESSION_CHANGE, WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT,
        WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    struct EventHostContext {
        on_event: Box<dyn Fn(HostEvent) + Send + 'static>,
        taskbar_created_msg: u32,
        battery_policy: BatteryPolicy,
        power_status: PowerStatus,
        is_locked: bool,
        is_remote: bool,
        is_display_off: bool,
        is_suspended: bool,
        last_global_reasons: PauseReason,
    }

    impl EventHostContext {
        fn recompute_and_dispatch(&mut self) {
            let battery_reasons = evaluate_battery_pause(self.battery_policy, &self.power_status);
            let session_reasons = evaluate_session_pause(
                self.is_locked,
                self.is_remote,
                self.is_display_off,
                self.is_suspended,
            );
            let new_reasons = battery_reasons | session_reasons;
            if new_reasons != self.last_global_reasons {
                self.last_global_reasons = new_reasons;
                (self.on_event)(HostEvent::GlobalPauseReasonsChanged(new_reasons));
            }
        }
    }

    pub struct ArcPaperEventHost {
        hwnd: HWND,
        ctx: *mut EventHostContext,
        power_notify_handles: Vec<HPOWERNOTIFY>,
        wts_registered: bool,
    }

    unsafe impl Send for ArcPaperEventHost {}

    unsafe extern "system" fn event_host_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, EndMenu, SetWindowLongPtrW};

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
                let ctx = &mut *(ctx_ptr as *mut EventHostContext);

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
                } else if msg == WM_WTSSESSION_CHANGE {
                    let event_code = wparam.0 as u32;
                    match event_code {
                        WTS_SESSION_LOCK => ctx.is_locked = true,
                        WTS_SESSION_UNLOCK => ctx.is_locked = false,
                        WTS_REMOTE_CONNECT => ctx.is_remote = true,
                        WTS_REMOTE_DISCONNECT => ctx.is_remote = false,
                        WTS_CONSOLE_DISCONNECT => ctx.is_remote = true,
                        WTS_CONSOLE_CONNECT => ctx.is_remote = false,
                        _ => {}
                    }
                    ctx.recompute_and_dispatch();
                    return LRESULT(0);
                } else if msg == WM_POWERBROADCAST {
                    let event_code = wparam.0 as u32;
                    match event_code {
                        PBT_APMSUSPEND => {
                            ctx.is_suspended = true;
                            ctx.recompute_and_dispatch();
                            return LRESULT(1);
                        }
                        PBT_APMRESUMEAUTOMATIC => {
                            ctx.is_suspended = false;
                            ctx.last_global_reasons =
                                evaluate_battery_pause(ctx.battery_policy, &ctx.power_status)
                                    | evaluate_session_pause(
                                        ctx.is_locked,
                                        ctx.is_remote,
                                        ctx.is_display_off,
                                        false,
                                    );
                            (ctx.on_event)(HostEvent::ResumeFromSuspend);
                            return LRESULT(1);
                        }
                        PBT_POWERSETTINGCHANGE => {
                            if lparam.0 != 0 {
                                let setting = &*(lparam.0
                                    as *const windows::Win32::System::Power::POWERBROADCAST_SETTING);
                                let guid = setting.PowerSetting;
                                let data_ptr = setting.Data.as_ptr();
                                if setting.DataLength >= 4 {
                                    let val = *(data_ptr as *const u32);
                                    if guid
                                        == windows::Win32::System::SystemServices::GUID_ACDC_POWER_SOURCE
                                    {
                                        // 0 = PoAc (AC power), 1 = PoDc (Battery), 2 = PoHot
                                        ctx.power_status.ac_online = val == 0;
                                        if val == 1 || val == 2 {
                                            ctx.power_status.has_battery = true;
                                        }
                                        ctx.recompute_and_dispatch();
                                    } else if guid
                                        == windows::Win32::System::SystemServices::GUID_POWER_SAVING_STATUS
                                    {
                                        // 0 = Battery Saver off, 1 = Battery Saver on
                                        ctx.power_status.battery_saver = val != 0;
                                        ctx.recompute_and_dispatch();
                                    } else if guid
                                        == windows::Win32::System::SystemServices::GUID_BATTERY_PERCENTAGE_REMAINING
                                    {
                                        ctx.power_status.battery_percent =
                                            if val <= 100 { Some(val as u8) } else { None };
                                        ctx.power_status.has_battery = true;
                                        ctx.recompute_and_dispatch();
                                    } else if guid
                                        == windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE
                                    {
                                        // 0 = PowerMonitorOff, 1 = PowerMonitorOn, 2 = PowerMonitorDim
                                        ctx.is_display_off = val == 0;
                                        ctx.recompute_and_dispatch();
                                    }
                                }
                            }
                            return LRESULT(1);
                        }
                        _ => {}
                    }
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

                let probe = WindowsSystemProbe::new();
                let power_status = probe.get_power_status();
                let is_remote = probe.is_remote_session();
                let battery_policy = BatteryPolicy::PauseOnBatterySaver;
                let battery_reasons = evaluate_battery_pause(battery_policy, &power_status);
                let session_reasons = evaluate_session_pause(false, is_remote, false, false);
                let initial_reasons = battery_reasons | session_reasons;

                let ctx = Box::into_raw(Box::new(EventHostContext {
                    on_event: Box::new(on_event),
                    taskbar_created_msg,
                    battery_policy,
                    power_status,
                    is_locked: false,
                    is_remote,
                    is_display_off: false,
                    is_suspended: false,
                    last_global_reasons: initial_reasons,
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

                let wts_registered =
                    WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION).is_ok();

                let mut power_notify_handles = Vec::new();
                let guids = [
                    windows::Win32::System::SystemServices::GUID_ACDC_POWER_SOURCE,
                    windows::Win32::System::SystemServices::GUID_POWER_SAVING_STATUS,
                    windows::Win32::System::SystemServices::GUID_BATTERY_PERCENTAGE_REMAINING,
                    windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE,
                ];

                for guid in guids {
                    let handle = RegisterPowerSettingNotification(
                        HANDLE(hwnd.0),
                        &guid,
                        DEVICE_NOTIFY_WINDOW_HANDLE,
                    );
                    if let Ok(h) = handle {
                        power_notify_handles.push(h);
                    }
                }

                if !initial_reasons.is_empty() {
                    ((*ctx).on_event)(HostEvent::GlobalPauseReasonsChanged(initial_reasons));
                }

                Ok(Self {
                    hwnd,
                    ctx,
                    power_notify_handles,
                    wts_registered,
                })
            }
        }

        /// Returns the underlying window handle.
        pub fn hwnd(&self) -> HWND {
            self.hwnd
        }

        /// Returns the current active global pause reasons.
        pub fn current_global_reasons(&self) -> PauseReason {
            unsafe {
                if !self.ctx.is_null() {
                    (*self.ctx).last_global_reasons
                } else {
                    PauseReason::empty()
                }
            }
        }

        /// Sets the active battery policy and immediately re-evaluates pause state.
        pub fn set_battery_policy(&mut self, policy: BatteryPolicy) {
            unsafe {
                if !self.ctx.is_null() {
                    (*self.ctx).battery_policy = policy;
                    (*self.ctx).recompute_and_dispatch();
                }
            }
        }
    }

    impl Drop for ArcPaperEventHost {
        fn drop(&mut self) {
            unsafe {
                for handle in self.power_notify_handles.drain(..) {
                    let _ = UnregisterPowerSettingNotification(handle);
                }
                if self.wts_registered && !self.hwnd.0.is_null() {
                    let _ = WTSUnRegisterSessionNotification(self.hwnd);
                    self.wts_registered = false;
                }
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

    pub fn current_global_reasons(&self) -> PauseReason {
        PauseReason::empty()
    }

    pub fn set_battery_policy(&mut self, _policy: crate::power::BatteryPolicy) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_event_equality_and_debug() {
        let e1 = HostEvent::DisplayTopologyDirty;
        let e2 = HostEvent::DisplayTopologyDirty;
        let e3 = HostEvent::ExplorerRestarted;
        let e4 = HostEvent::GlobalPauseReasonsChanged(PauseReason::BATTERY);
        let e5 = HostEvent::ResumeFromSuspend;

        assert_eq!(e1, e2);
        assert_ne!(e1, e3);
        assert_ne!(e1, e4);
        assert_ne!(e4, e5);

        assert_eq!(format!("{:?}", e1), "DisplayTopologyDirty");
        assert_eq!(format!("{:?}", e3), "ExplorerRestarted");
        assert_eq!(
            format!("{:?}", e4),
            "GlobalPauseReasonsChanged(PauseReason(BATTERY))"
        );
        assert_eq!(format!("{:?}", e5), "ResumeFromSuspend");

        let cloned = e4;
        assert_eq!(cloned, e4);
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
        let mut host = host.unwrap();
        assert!(!host.hwnd().0.is_null());

        let reasons = host.current_global_reasons();
        assert!(reasons.is_global() || reasons.is_empty());

        host.set_battery_policy(crate::power::BatteryPolicy::AlwaysPlay);

        drop(host);
    }
}
