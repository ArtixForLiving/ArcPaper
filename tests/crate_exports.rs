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

    #[cfg(windows)]
    {
        // Ensure workerw symbol is reachable from the library target.
        let _fn_ptr: fn() -> _ = workerw::get_progman_hwnd;
    }
}
