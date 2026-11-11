use arc_paper::arc_player::{LoopController, ScreenFit};
#[cfg(windows)]
use arc_paper::workerw;

#[test]
fn library_crate_exports_core_modules() {
    let mut ctrl = LoopController::new(2);
    let (idx0, pts0, wrap0) = ctrl.advance(100);
    assert_eq!((idx0, pts0, wrap0), (0, 0, false));
    assert_eq!(ScreenFit::from_str_mode("contain"), ScreenFit::Contain);

    #[cfg(windows)]
    {
        // Ensure workerw symbol is reachable from the library target.
        let _fn_ptr: fn() -> _ = workerw::get_progman_hwnd;
    }
}
