//! ArcPlayer: High-performance, zero-copy, low-memory native Windows video engine.

#[cfg(windows)]
pub mod autopause;
#[cfg(windows)]
pub mod decoder;
pub mod demux;
pub mod loop_ctrl;
#[cfg(windows)]
pub mod presenter;
#[cfg(windows)]
pub mod renderer;

#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::{Arc, Condvar, Mutex};
#[cfg(windows)]
use std::thread::{self, JoinHandle};
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;
#[cfg(windows)]
use windows::Win32::Foundation::HWND;

#[cfg(windows)]
use crate::app::log::{LogLevel, log_throttled, log_with_level};
#[cfg(windows)]
pub use autopause::{AutoPause, FramePacer};
#[cfg(windows)]
pub use decoder::{DecodedFrame, Decoder};
pub use demux::{Demuxer, VideoCodec};
pub use loop_ctrl::LoopController;
#[cfg(windows)]
pub use presenter::{Presenter, is_device_lost_error, is_device_lost_hresult};
#[cfg(windows)]
pub use renderer::Renderer;

/// Video display scaling modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenFit {
    #[default]
    Cover,
    Contain,
    Fill,
}

impl ScreenFit {
    pub fn from_str_mode(mode: &str) -> Self {
        match mode.to_lowercase().as_str() {
            "fill" => ScreenFit::Fill,
            "contain" => ScreenFit::Contain,
            _ => ScreenFit::Cover,
        }
    }
}

/// Shared command state exchanged between the control facade and the playback worker thread.
#[cfg(windows)]
struct SharedState {
    video_path: Option<PathBuf>,
    is_paused: bool,
    fit_mode: ScreenFit,
    brightness: f32,
    speed: f32,
    target_fps: Option<u32>,
    resize_req: Option<(u32, u32)>,
    native_fps: Option<f64>,
    is_running: bool,
}

/// Tracks consecutive GPU device-lost recovery attempts and backoff intervals.
#[derive(Debug, Default)]
pub struct RecoveryTracker {
    consecutive_attempts: u32,
    exhausted: bool,
}

impl RecoveryTracker {
    pub const MAX_ATTEMPTS: u32 = 3;

    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the backoff sleep duration for the next recovery attempt,
    /// or `None` once `MAX_ATTEMPTS` consecutive recoveries have failed.
    pub fn next_backoff(&mut self) -> Option<Duration> {
        if self.exhausted || self.consecutive_attempts >= Self::MAX_ATTEMPTS {
            self.exhausted = true;
            return None;
        }
        self.consecutive_attempts += 1;
        let ms = match self.consecutive_attempts {
            1 => 150,
            2 => 400,
            _ => 800,
        };
        Some(Duration::from_millis(ms))
    }

    /// Resets the failure counter after a recovered frame presents cleanly.
    pub fn reset(&mut self) {
        self.consecutive_attempts = 0;
        self.exhausted = false;
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    pub fn attempts(&self) -> u32 {
        self.consecutive_attempts
    }
}

/// Resets an exhausted recovery tracker and requests pipeline rebuild when a new video loads
/// while no presenter is currently active.
pub fn prepare_recovery_on_video_load(has_presenter: bool, recovery: &mut RecoveryTracker) -> bool {
    if !has_presenter || recovery.is_exhausted() {
        recovery.reset();
        return !has_presenter;
    }
    false
}

/// Executes bounded recovery retries using `RecoveryTracker`, sleeping between attempts.
pub fn run_recovery_with<T, E, S, R>(
    tracker: &mut RecoveryTracker,
    mut sleep_fn: S,
    mut rebuild_fn: R,
) -> Result<T, Option<E>>
where
    S: FnMut(Duration),
    R: FnMut() -> Result<T, E>,
{
    let mut last_err = None;
    while let Some(backoff) = tracker.next_backoff() {
        sleep_fn(backoff);
        match rebuild_fn() {
            Ok(val) => return Ok(val),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err)
}

#[cfg(windows)]
fn snap_to_nearest_keyframe(demux: &Demuxer, lctrl: &mut LoopController) {
    let frames = demux.frames();
    if frames.is_empty() {
        return;
    }
    let idx = lctrl.current_frame_idx.min(frames.len() - 1);
    let key_idx = frames[..=idx]
        .iter()
        .rposition(|f| f.is_keyframe)
        .unwrap_or(0);
    lctrl.current_frame_idx = key_idx;
}

#[cfg(windows)]
fn build_d3d_pipeline(
    hwnd: HWND,
    width: u32,
    height: u32,
    demuxer: Option<&Demuxer>,
    loop_ctrl: Option<&mut LoopController>,
) -> windows::core::Result<(Presenter, Renderer, Option<Decoder>)> {
    let pres = Presenter::new(hwnd, width, height)?;
    let rend = Renderer::new(pres.device())?;
    let dec = if let Some(demux) = demuxer {
        if let Some(lctrl) = loop_ctrl {
            snap_to_nearest_keyframe(demux, lctrl);
        }
        let codec = demux.codec().unwrap_or(VideoCodec::H264);
        Some(Decoder::new(
            pres.device(),
            codec,
            demux.width(),
            demux.height(),
        )?)
    } else {
        None
    };
    Ok((pres, rend, dec))
}

/// Main ArcPlayer coordinator orchestrating playback via a dedicated background rendering thread.
#[cfg(windows)]
pub struct ArcPlayer {
    state: Arc<(Mutex<SharedState>, Condvar)>,
    thread_handle: Option<JoinHandle<()>>,
}

#[cfg(windows)]
impl ArcPlayer {
    /// Spawns a dedicated playback thread attached to the target WorkerW desktop window.
    pub fn new(hwnd: HWND, initial_width: u32, initial_height: u32) -> Result<Self, String> {
        let initial_state = SharedState {
            video_path: None,
            is_paused: false,
            fit_mode: ScreenFit::Cover,
            brightness: 1.0,
            speed: 1.0,
            target_fps: None,
            resize_req: None,
            native_fps: None,
            is_running: true,
        };

        let shared = Arc::new((Mutex::new(initial_state), Condvar::new()));
        let thread_shared = Arc::clone(&shared);
        let hwnd_raw = hwnd.0 as usize;

        let thread_handle = thread::Builder::new()
            .name("ArcPlayer-RenderLoop".to_string())
            .spawn(move || {
                let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
                Self::playback_thread_main(hwnd, initial_width, initial_height, thread_shared);
            })
            .map_err(|e| format!("Failed to spawn playback thread: {}", e))?;

        Ok(Self {
            state: shared,
            thread_handle: Some(thread_handle),
        })
    }

    /// Background worker thread managing the entire decoding and presentation pipeline.
    fn playback_thread_main(
        hwnd: HWND,
        width: u32,
        height: u32,
        shared: Arc<(Mutex<SharedState>, Condvar)>,
    ) {
        // Explicit COM & Media Foundation initialization on this worker thread
        unsafe {
            use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        }

        let (lock, cvar) = &*shared;
        let mut cur_width = width.max(1);
        let mut cur_height = height.max(1);

        // 1. Initialize Direct3D 11 Presenter, Renderer, and FramePacer
        let mut presenter = match Presenter::new(hwnd, cur_width, cur_height) {
            Ok(p) => Some(p),
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!(
                        "[ArcPlayer] Initial Presenter::new failed ({:?}): {}",
                        e.code(),
                        e
                    ),
                );
                return;
            }
        };

        let mut renderer = match Renderer::new(presenter.as_ref().unwrap().device()) {
            Ok(r) => Some(r),
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!(
                        "[ArcPlayer] Initial Renderer::new failed ({:?}): {}",
                        e.code(),
                        e
                    ),
                );
                return;
            }
        };

        let pacer = match FramePacer::new() {
            Ok(p) => p,
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!("[ArcPlayer] FramePacer::new failed ({:?}): {}", e.code(), e),
                );
                return;
            }
        };

        let mut loaded_path: Option<PathBuf> = None;
        let mut demuxer: Option<Demuxer> = None;
        let mut loop_ctrl: Option<LoopController> = None;
        let mut decoder: Option<Decoder> = None;
        let mut was_paused = false;
        let mut recovery = RecoveryTracker::new();

        let mut first_video_load_time: Option<Instant> = None;
        let mut initial_trim_done = false;

        loop {
            // Read snapshot of current command state
            let (
                video_to_load,
                is_paused,
                fit_mode,
                brightness,
                speed,
                target_fps,
                resize_req,
                is_running,
            ) = {
                let mut state = lock.lock().unwrap();
                if !state.is_running {
                    break;
                }

                let resize_req = state.resize_req.take();
                (
                    state.video_path.clone(),
                    state.is_paused,
                    state.fit_mode,
                    state.brightness,
                    state.speed,
                    state.target_fps,
                    resize_req,
                    state.is_running,
                )
            };

            if !is_running {
                break;
            }

            // Drain Win32 message queue for this thread and its child window to ensure hit-testing
            // and cursor updates are never blocked or marked as "Not Responding" by Windows
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
                };
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            let mut needs_device_recovery = false;

            // Handle window resize requests
            if let Some((new_w, new_h)) = resize_req {
                if new_w > 0 && new_h > 0 {
                    cur_width = new_w;
                    cur_height = new_h;
                }
                if let Some(ref mut pres) = presenter
                    && let Err(e) = pres.resize(new_w, new_h)
                {
                    log_throttled(
                        "resize_err",
                        Duration::from_secs(5),
                        LogLevel::Error,
                        &format!(
                            "[ArcPlayer] Presenter::resize failed ({:?}): {}",
                            e.code(),
                            e
                        ),
                    );
                    if is_device_lost_error(&e)
                        || is_device_lost_hresult(pres.device_removed_reason())
                    {
                        needs_device_recovery = true;
                    }
                }
            }

            // Handle loading a new video file
            if !needs_device_recovery && video_to_load != loaded_path {
                loaded_path = video_to_load.clone();
                if let Some(ref path) = video_to_load {
                    let mut d = Demuxer::new();
                    match d.open(path) {
                        Ok(()) => {
                            let frame_count = d.frame_count();
                            let codec = d.codec().unwrap_or(VideoCodec::H264);
                            let video_w = d.width();
                            let video_h = d.height();

                            let native_fps = if frame_count > 0 && d.frames()[0].duration_100ns > 0
                            {
                                10_000_000.0 / (d.frames()[0].duration_100ns as f64)
                            } else {
                                60.0
                            };

                            let dec = if let Some(ref pres) = presenter {
                                match Decoder::new(pres.device(), codec, video_w, video_h) {
                                    Ok(dec) => Some(dec),
                                    Err(e) => {
                                        log_with_level(
                                            LogLevel::Error,
                                            &format!(
                                                "[ArcPlayer] Decoder::new failed ({:?}): {}",
                                                e.code(),
                                                e
                                            ),
                                        );
                                        if is_device_lost_error(&e) {
                                            needs_device_recovery = true;
                                        }
                                        None
                                    }
                                }
                            } else {
                                None
                            };

                            let mut state = lock.lock().unwrap();
                            state.native_fps = Some(native_fps);

                            loop_ctrl = Some(LoopController::new(frame_count));
                            decoder = dec;
                            demuxer = Some(d);

                            if prepare_recovery_on_video_load(presenter.is_some(), &mut recovery) {
                                needs_device_recovery = true;
                            }

                            if first_video_load_time.is_none() {
                                first_video_load_time = Some(Instant::now());
                            }
                        }
                        Err(e) => {
                            log_with_level(
                                LogLevel::Error,
                                &format!("[ArcPlayer] Demuxer::open({:?}) failed: {}", path, e),
                            );
                            demuxer = None;
                            decoder = None;
                            loop_ctrl = None;
                        }
                    }
                } else {
                    demuxer = None;
                    decoder = None;
                    loop_ctrl = None;
                }
            }

            // Execute one-shot initialization trim 3 seconds after first video loads
            if !initial_trim_done
                && first_video_load_time.is_some_and(|t| t.elapsed() >= Duration::from_secs(3))
            {
                AutoPause::trim_working_set();
                initial_trim_done = true;
            }

            // Handle paused / hibernation mode
            if !needs_device_recovery && is_paused {
                if !was_paused {
                    if let Some(ref pres) = presenter {
                        AutoPause::enter_hibernation(pres);
                    }
                    was_paused = true;
                }

                let mut state = lock.lock().unwrap();
                while state.is_paused
                    && state.is_running
                    && state.video_path == loaded_path
                    && state.resize_req.is_none()
                {
                    let (new_state, _) =
                        cvar.wait_timeout(state, Duration::from_millis(50)).unwrap();
                    state = new_state;

                    // Drain Win32 message queue while paused to ensure continuous responsiveness
                    unsafe {
                        use windows::Win32::UI::WindowsAndMessaging::{
                            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
                        };
                        let mut msg = MSG::default();
                        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
                continue;
            } else if was_paused {
                was_paused = false;
            }

            // Perform single playback tick
            if !needs_device_recovery
                && !recovery.is_exhausted()
                && let (Some(demux), Some(lctrl), Some(dec), Some(pres), Some(rend)) = (
                    &mut demuxer,
                    &mut loop_ctrl,
                    &mut decoder,
                    &presenter,
                    &renderer,
                )
            {
                match demux.read_next_annexb_packet(lctrl) {
                    Ok((packet, pts_100ns, duration_100ns, _)) => {
                        let base_interval_100ns = if let Some(target_fps) = target_fps {
                            let target_interval = (10_000_000 / target_fps.max(1)) as i64;
                            duration_100ns.max(target_interval)
                        } else {
                            duration_100ns
                        };

                        let effective_sleep_100ns =
                            (base_interval_100ns as f64 / (speed as f64).max(0.1)) as i64;

                        pacer.sleep_100ns(effective_sleep_100ns);

                        match dec.feed_and_decode(packet, pts_100ns, duration_100ns) {
                            Ok(Some(frame)) => {
                                if let Err(e) = rend.render_nv12_frame(
                                    pres,
                                    &frame.texture,
                                    frame.subresource_index,
                                    demux.width(),
                                    demux.height(),
                                    brightness,
                                    fit_mode,
                                ) {
                                    log_throttled(
                                        "render_err",
                                        Duration::from_secs(5),
                                        LogLevel::Error,
                                        &format!(
                                            "[ArcPlayer] render_nv12_frame error ({:?}): {}",
                                            e.code(),
                                            e
                                        ),
                                    );
                                    if is_device_lost_error(&e)
                                        || is_device_lost_hresult(pres.device_removed_reason())
                                    {
                                        needs_device_recovery = true;
                                    }
                                } else if let Err(e) = pres.present(true) {
                                    log_throttled(
                                        "present_err",
                                        Duration::from_secs(5),
                                        LogLevel::Error,
                                        &format!(
                                            "[ArcPlayer] SwapChain Present error ({:?}): {}",
                                            e.code(),
                                            e
                                        ),
                                    );
                                    if is_device_lost_error(&e)
                                        || is_device_lost_hresult(pres.device_removed_reason())
                                    {
                                        needs_device_recovery = true;
                                    }
                                } else if recovery.attempts() > 0 {
                                    recovery.reset();
                                    log_with_level(
                                        LogLevel::Info,
                                        "[ArcPlayer] GPU pipeline resumed cleanly after device recovery.",
                                    );
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                log_throttled(
                                    "decode_err",
                                    Duration::from_secs(5),
                                    LogLevel::Error,
                                    &format!(
                                        "[ArcPlayer] feed_and_decode error ({:?}): {}",
                                        e.code(),
                                        e
                                    ),
                                );
                                if is_device_lost_error(&e)
                                    || is_device_lost_hresult(pres.device_removed_reason())
                                {
                                    needs_device_recovery = true;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log_throttled(
                            "demux_read_err",
                            Duration::from_secs(5),
                            LogLevel::Warn,
                            &format!("[ArcPlayer] Packet read error: {e}"),
                        );
                        pacer.sleep_duration(Duration::from_millis(16));
                    }
                }
            } else if !needs_device_recovery {
                pacer.sleep_duration(Duration::from_millis(if recovery.is_exhausted() {
                    250
                } else {
                    33
                }));
            }

            if needs_device_recovery {
                let reason = presenter
                    .as_ref()
                    .map(|p| p.device_removed_reason())
                    .unwrap_or(windows::Win32::Foundation::E_FAIL);
                log_with_level(
                    LogLevel::Error,
                    &format!(
                        "[ArcPlayer] DXGI device lost detected (reason: {:?}). Releasing D3D11/MF resources for recovery...",
                        reason
                    ),
                );

                // Strictly drop COM dependents before releasing Presenter/Device/ChildHWND
                decoder = None;
                renderer = None;
                presenter = None;

                match run_recovery_with(
                    &mut recovery,
                    |d| pacer.sleep_duration(d),
                    || {
                        build_d3d_pipeline(
                            hwnd,
                            cur_width,
                            cur_height,
                            demuxer.as_ref(),
                            loop_ctrl.as_mut(),
                        )
                    },
                ) {
                    Ok((new_pres, new_rend, new_dec)) => {
                        presenter = Some(new_pres);
                        renderer = Some(new_rend);
                        decoder = new_dec;
                        log_with_level(
                            LogLevel::Info,
                            &format!(
                                "[ArcPlayer] D3D11 pipeline recreated on attempt {}/{}.",
                                recovery.attempts(),
                                RecoveryTracker::MAX_ATTEMPTS
                            ),
                        );
                    }
                    Err(last_err) => {
                        log_with_level(
                            LogLevel::Error,
                            &format!(
                                "[ArcPlayer] D3D11 recovery exhausted after {} attempts (last error: {:?}).",
                                RecoveryTracker::MAX_ATTEMPTS,
                                last_err.map(|e| e.code())
                            ),
                        );
                    }
                }
            }
        }

        // Cleanup upon thread exit
        decoder = None;
        renderer = None;
        if let Some(ref pres) = presenter {
            AutoPause::enter_hibernation(pres);
        }
        drop(decoder);
        drop(renderer);
        drop(presenter);
        unsafe {
            use windows::Win32::Media::MediaFoundation::MFShutdown;
            use windows::Win32::System::Com::CoUninitialize;
            let _ = MFShutdown();
            CoUninitialize();
        }
    }

    /// Loads a video wallpaper from disk.
    pub fn load_file<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.video_path = Some(path.as_ref().to_path_buf());
        state.native_fps = None;
        cvar.notify_all();
        Ok(())
    }

    /// Pauses or resumes wallpaper playback.
    pub fn set_pause(&self, paused: bool) -> Result<(), String> {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().unwrap();
        if state.is_paused != paused {
            state.is_paused = paused;
            cvar.notify_all();
        }
        Ok(())
    }

    /// Returns whether the player is currently paused.
    pub fn is_paused(&self) -> bool {
        let (lock, _) = &*self.state;
        lock.lock().unwrap().is_paused
    }

    /// Adjusts the playback speed multiplier (e.g. 0.5, 1.0, 1.5, 2.0).
    pub fn set_speed(&self, speed: f32) -> Result<(), String> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.speed = speed.max(0.1);
        Ok(())
    }

    /// Sets the display brightness factor. Accepts UI values where 100 is neutral (1.0).
    pub fn set_brightness(&self, brightness: u32) -> Result<(), String> {
        let factor = (brightness as f32) / 100.0;
        let (lock, _) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.brightness = factor.clamp(0.0, 2.0);
        Ok(())
    }

    /// Sets the screen fit mode string ("cover", "contain", "fill").
    pub fn set_fit(&self, fit_mode: &str) -> Result<(), String> {
        let mode = ScreenFit::from_str_mode(fit_mode);
        self.set_fit_mode(mode);
        Ok(())
    }

    /// Sets the structured screen fit scaling mode.
    pub fn set_fit_mode(&self, fit: ScreenFit) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.fit_mode = fit;
    }

    /// Configures an optional target FPS cap (e.g. 30, 60, or None for native).
    pub fn apply_fps_limit(&self, target_fps: Option<u32>) -> Result<(), String> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.target_fps = target_fps;
        Ok(())
    }

    /// Detects and returns the native frame rate of the currently loaded video stream.
    pub fn detect_native_fps(&self) -> Option<f64> {
        for _ in 0..30 {
            {
                let (lock, _) = &*self.state;
                if let Some(fps) = lock.lock().unwrap().native_fps {
                    return Some(fps);
                }
            }
            thread::sleep(Duration::from_millis(15));
        }
        let (lock, _) = &*self.state;
        lock.lock().unwrap().native_fps
    }

    /// Queues a SwapChain buffer resize request.
    pub fn resize(&self, width: u32, height: u32) -> Result<(), String> {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.resize_req = Some((width, height));
        cvar.notify_all();
        Ok(())
    }

    /// No-op in ArcPlayer: pure Rust architecture eliminates unmanaged event queues.
    pub fn drain_events(&self) {}
}

#[cfg(windows)]
impl Drop for ArcPlayer {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.state;
            let mut state = lock.lock().unwrap();
            state.is_running = false;
            cvar.notify_all();
        }

        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn test_arc_player_control_api() {
        let initial_state = SharedState {
            video_path: None,
            is_paused: false,
            fit_mode: ScreenFit::Cover,
            brightness: 1.0,
            speed: 1.0,
            target_fps: None,
            resize_req: None,
            native_fps: None,
            is_running: true,
        };
        let shared = Arc::new((Mutex::new(initial_state), Condvar::new()));
        let player = ArcPlayer {
            state: shared,
            thread_handle: None,
        };

        player.set_pause(true).unwrap();
        assert!(player.is_paused());

        player.set_speed(1.5).unwrap();
        player.set_brightness(80).unwrap();
        player.set_fit("contain").unwrap();
        player.apply_fps_limit(Some(30)).unwrap();

        let (lock, _) = &*player.state;
        let s = lock.lock().unwrap();
        assert_eq!(s.fit_mode, ScreenFit::Contain);
        assert_eq!(s.speed, 1.5);
        assert!((s.brightness - 0.8).abs() < 1e-4);
        assert_eq!(s.target_fps, Some(30));
    }

    #[test]
    fn test_device_recovery_tracker_bounds_and_reset() {
        let mut tracker = RecoveryTracker::new();
        assert!(!tracker.is_exhausted());

        assert_eq!(tracker.next_backoff(), Some(Duration::from_millis(150)));
        assert_eq!(tracker.attempts(), 1);
        assert_eq!(tracker.next_backoff(), Some(Duration::from_millis(400)));
        assert_eq!(tracker.attempts(), 2);
        assert_eq!(tracker.next_backoff(), Some(Duration::from_millis(800)));
        assert_eq!(tracker.attempts(), 3);

        // 4th consecutive attempt must stop and mark exhausted
        assert_eq!(tracker.next_backoff(), None);
        assert!(tracker.is_exhausted());

        // Successful frame present resets consecutive attempt counter
        tracker.reset();
        assert!(!tracker.is_exhausted());
        assert_eq!(tracker.attempts(), 0);
        assert_eq!(tracker.next_backoff(), Some(Duration::from_millis(150)));
    }

    #[test]
    fn test_run_recovery_with_bounded_retries_and_exhaustion() {
        let mut tracker = RecoveryTracker::new();
        let mut slept = Vec::new();
        let mut calls = 0u32;

        let ok_res: Result<&str, Option<&str>> = run_recovery_with(
            &mut tracker,
            |d| slept.push(d),
            || {
                calls += 1;
                if calls < 3 {
                    Err("device still resetting")
                } else {
                    Ok("recovered")
                }
            },
        );
        assert_eq!(ok_res, Ok("recovered"));
        assert_eq!(calls, 3);
        assert_eq!(
            slept,
            vec![
                Duration::from_millis(150),
                Duration::from_millis(400),
                Duration::from_millis(800),
            ]
        );
        assert!(!tracker.is_exhausted());

        // Exhaustion after 3 consecutive failures
        let mut tracker2 = RecoveryTracker::new();
        let mut calls2 = 0u32;
        let fail_res: Result<(), Option<&str>> = run_recovery_with(
            &mut tracker2,
            |_| {},
            || {
                calls2 += 1;
                Err("permanent gpu loss")
            },
        );
        assert_eq!(fail_res, Err(Some("permanent gpu loss")));
        assert_eq!(calls2, 3);
        assert!(tracker2.is_exhausted());
    }

    #[test]
    fn test_video_load_resets_exhausted_recovery_when_presenter_missing() {
        let mut healthy = RecoveryTracker::new();
        assert!(!prepare_recovery_on_video_load(true, &mut healthy));
        assert!(!healthy.is_exhausted());

        let mut exhausted = RecoveryTracker::new();
        for _ in 0..4 {
            let _ = exhausted.next_backoff();
        }
        assert!(exhausted.is_exhausted());

        assert!(prepare_recovery_on_video_load(false, &mut exhausted));
        assert!(!exhausted.is_exhausted());
        assert_eq!(exhausted.attempts(), 0);
    }
}
