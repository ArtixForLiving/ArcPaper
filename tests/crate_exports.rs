use arc_paper::arc_player::{LoopController, ScreenFit};
#[cfg(windows)]
use arc_paper::workerw;
use arc_paper::{ArcPaperEventHost, HostEvent, PauseReason};

#[test]
fn library_crate_exports_core_modules() {
    let mut ctrl = LoopController::new(2);
    let (idx0, pts0, wrap0) = ctrl.advance(100);
    assert_eq!((idx0, pts0, wrap0), (0, 0, false));
    assert_eq!(ScreenFit::from_str_mode("contain"), ScreenFit::Contain);

    // Verify PauseReason re-exports
    assert_eq!(PauseReason::default(), arc_paper::app::PauseReason::empty());

    // Verify HostEvent and ArcPaperEventHost re-exports
    assert_eq!(
        HostEvent::DisplayTopologyDirty,
        arc_paper::app::HostEvent::DisplayTopologyDirty
    );
    let _ = ArcPaperEventHost::new(|_event| {});

    // Verify winevent re-export
    assert_eq!(
        arc_paper::winevent::WINEVENT_THREAD_NAME,
        "ArcPaper-WinEventHook"
    );

    // Verify power module re-exports
    assert_eq!(
        arc_paper::BatteryPolicy::default(),
        arc_paper::power::BatteryPolicy::PauseOnBatterySaver
    );
    assert_eq!(
        arc_paper::evaluate_session_pause(false, false, false, false),
        arc_paper::PauseReason::empty()
    );
    assert!(!arc_paper::is_quns_exclusive_fullscreen(
        arc_paper::power::QUNS_NOT_PRESENT,
        false
    ));
    assert!(!arc_paper::is_reduce_motion_enabled(false));

    #[cfg(windows)]
    {
        // Ensure workerw symbol is reachable from the library target.
        let _fn_ptr: fn() -> _ = workerw::get_progman_hwnd;

        use windows::Win32::System::RemoteDesktop::*;
        use windows::Win32::System::SystemServices::*;
        use windows::Win32::UI::WindowsAndMessaging::*;
        let _ = NOTIFY_FOR_THIS_SESSION;
        let _ = WTS_SESSION_LOCK;
        let _ = WTS_SESSION_UNLOCK;
        let _ = WTS_REMOTE_CONNECT;
        let _ = WTS_REMOTE_DISCONNECT;
        let _ = WTS_CONSOLE_DISCONNECT;
        let _ = GUID_ACDC_POWER_SOURCE;
        let _ = arc_paper::power::GUID_ENERGY_SAVER_STATUS;
        let _ = GUID_BATTERY_PERCENTAGE_REMAINING;
        let _ = GUID_CONSOLE_DISPLAY_STATE;
        let _ = PBT_POWERSETTINGCHANGE;
        let _ = PBT_APMSUSPEND;
        let _ = PBT_APMRESUMEAUTOMATIC;
    }
}
