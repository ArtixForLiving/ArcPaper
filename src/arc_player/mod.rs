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
use std::collections::{BTreeMap, HashSet};
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

/// Specifications for wallpaper playback on a single monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorPlaybackSpec {
    pub monitor_id: crate::display::MonitorId,
    pub video_path: Option<PathBuf>,
    pub is_paused: bool,
    pub brightness: f32,
    pub fit_mode: ScreenFit,
    pub speed: f32,
    pub target_fps: Option<u32>,
    pub is_primary: bool,
    pub rect: crate::display::DisplayRect,
}

/// Consolidated decoding assignment for a unique video file shared across one or more monitors.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionAssignment {
    pub video_path: PathBuf,
    pub subscriber_ids: Vec<crate::display::MonitorId>,
    pub active_subscriber_ids: Vec<crate::display::MonitorId>,
    pub effective_speed: f32,
    pub effective_target_fps: Option<u32>,
}

/// Planned set of video decoding sessions and global activity status.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionPlan {
    pub sessions: Vec<SessionAssignment>,
    pub total_active_monitors: usize,
}

impl SessionPlan {
    /// Returns true when all monitors across all sessions are paused or inactive,
    /// commanding the engine to trim GPU and process working set pages.
    pub fn should_trim_global_memory(&self) -> bool {
        self.total_active_monitors == 0
    }

    /// Returns the number of currently active (non-paused) subscribers for the specified video path.
    pub fn active_subscribers_for(&self, video_path: &Path) -> usize {
        self.sessions
            .iter()
            .find(|s| s.video_path == video_path)
            .map(|s| s.active_subscriber_ids.len())
            .unwrap_or(0)
    }
}

/// Groups monitor playback specifications into deduplicated decoding sessions.
pub fn build_session_plan(specs: &[MonitorPlaybackSpec]) -> SessionPlan {
    let mut groups: std::collections::BTreeMap<PathBuf, Vec<&MonitorPlaybackSpec>> =
        std::collections::BTreeMap::new();

    for spec in specs {
        if let Some(ref path) = spec.video_path {
            groups.entry(path.clone()).or_default().push(spec);
        }
    }

    let mut sessions = Vec::new();
    let mut total_active_monitors = 0;

    for (video_path, group_specs) in groups {
        let mut ordered_specs = group_specs.clone();
        ordered_specs.sort_by(|a, b| match (a.is_primary, b.is_primary) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.monitor_id.cmp(&b.monitor_id),
        });

        let subscriber_ids: Vec<crate::display::MonitorId> =
            ordered_specs.iter().map(|s| s.monitor_id.clone()).collect();

        let active_specs: Vec<&MonitorPlaybackSpec> = ordered_specs
            .iter()
            .copied()
            .filter(|s| !s.is_paused)
            .collect();

        let active_subscriber_ids: Vec<crate::display::MonitorId> =
            active_specs.iter().map(|s| s.monitor_id.clone()).collect();

        total_active_monitors += active_subscriber_ids.len();

        let rep_spec = if !active_specs.is_empty() {
            active_specs[0]
        } else {
            ordered_specs[0]
        };

        sessions.push(SessionAssignment {
            video_path,
            subscriber_ids,
            active_subscriber_ids,
            effective_speed: rep_spec.speed,
            effective_target_fps: rep_spec.target_fps,
        });
    }

    SessionPlan {
        sessions,
        total_active_monitors,
    }
}

/// Pure-Rust device-wide recovery coordinator shared across all monitor sessions on one GraphicsDevice.
#[derive(Debug, Default)]
pub struct DeviceRecoveryCoordinator {
    epoch: u64,
    fault_flagged: bool,
    tracker: RecoveryTracker,
}

impl DeviceRecoveryCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn is_fault_flagged(&self) -> bool {
        self.fault_flagged
    }

    pub fn is_exhausted(&self) -> bool {
        self.tracker.is_exhausted()
    }

    pub fn attempts(&self) -> u32 {
        self.tracker.attempts()
    }

    /// Flags a device-lost event only if `fault_epoch == self.epoch` and not yet exhausted. Returns true if newly flagged.
    pub fn flag_fault(&mut self, fault_epoch: u64) -> bool {
        if fault_epoch == self.epoch && !self.tracker.is_exhausted() {
            if !self.fault_flagged {
                self.fault_flagged = true;
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Consumes the pending fault, advances the epoch, and returns the next backoff duration (or None if exhausted).
    pub fn begin_recovery_step(&mut self) -> Option<Duration> {
        self.fault_flagged = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.tracker.next_backoff()
    }

    /// Resets failure counter once pipeline recovers and presents cleanly.
    pub fn mark_healthy(&mut self) {
        self.tracker.reset();
        self.fault_flagged = false;
    }

    /// Resets exhausted recovery state when user loads a new video while device is down.
    pub fn reset_on_video_load(&mut self) {
        self.tracker.reset();
        self.fault_flagged = true;
    }
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

/// Subscriber display target receiving decoded frames from a shared video session.
#[cfg(windows)]
#[derive(Clone)]
pub struct SubscriberRenderInfo {
    pub monitor_id: crate::display::MonitorId,
    pub swap_chain: Arc<Mutex<Option<presenter::MonitorSwapChain>>>,
    pub rect: crate::display::DisplayRect,
    pub fit_mode: ScreenFit,
    pub brightness: f32,
    pub is_primary: bool,
}

#[cfg(windows)]
struct SharedSessionState {
    subscribers: Vec<SubscriberRenderInfo>,
    active_monitor_ids: Vec<crate::display::MonitorId>,
    speed: f32,
    target_fps: Option<u32>,
    display_mode: crate::app::config::DisplayMode,
    virtual_bounds: crate::display::VirtualDesktopBounds,
    is_running: bool,
    native_fps: Option<u32>,
}

/// Hardware video decoding session executing on a dedicated worker thread,
/// shared across all monitors configured to display the same video stream.
#[cfg(windows)]
pub struct SharedVideoSession {
    video_path: PathBuf,
    shared_state: Arc<(Mutex<SharedSessionState>, Condvar)>,
    thread_handle: Option<JoinHandle<()>>,
}

#[cfg(windows)]
impl SharedVideoSession {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        video_path: PathBuf,
        session_epoch: u64,
        gfx: Arc<presenter::GraphicsDevice>,
        renderer: Arc<Renderer>,
        recovery: Arc<Mutex<DeviceRecoveryCoordinator>>,
        subscribers: Vec<SubscriberRenderInfo>,
        active_monitor_ids: Vec<crate::display::MonitorId>,
        speed: f32,
        target_fps: Option<u32>,
        display_mode: crate::app::config::DisplayMode,
        virtual_bounds: crate::display::VirtualDesktopBounds,
    ) -> Result<Self, String> {
        let initial_state = SharedSessionState {
            subscribers,
            active_monitor_ids,
            speed,
            target_fps,
            display_mode,
            virtual_bounds,
            is_running: true,
            native_fps: None,
        };

        let shared = Arc::new((Mutex::new(initial_state), Condvar::new()));
        let thread_shared = Arc::clone(&shared);
        let path_clone = video_path.clone();

        let thread_name = format!(
            "ArcPaper-Session-{:?}",
            video_path.file_name().unwrap_or_default().to_string_lossy()
        );

        let thread_handle = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                Self::session_worker_main(
                    path_clone,
                    session_epoch,
                    gfx,
                    renderer,
                    recovery,
                    thread_shared,
                );
            })
            .map_err(|e| format!("Failed to spawn session thread: {}", e))?;

        Ok(Self {
            video_path,
            shared_state: shared,
            thread_handle: Some(thread_handle),
        })
    }

    pub fn update_subscribers(
        &self,
        subscribers: Vec<SubscriberRenderInfo>,
        active_monitor_ids: Vec<crate::display::MonitorId>,
        speed: f32,
        target_fps: Option<u32>,
        display_mode: crate::app::config::DisplayMode,
        virtual_bounds: crate::display::VirtualDesktopBounds,
    ) {
        let (lock, cvar) = &*self.shared_state;
        let mut state = lock.lock().unwrap();
        state.subscribers = subscribers;
        state.active_monitor_ids = active_monitor_ids;
        state.speed = speed;
        state.target_fps = target_fps;
        state.display_mode = display_mode;
        state.virtual_bounds = virtual_bounds;
        cvar.notify_all();
    }

    pub fn native_fps(&self) -> Option<u32> {
        let (lock, _) = &*self.shared_state;
        lock.lock().unwrap().native_fps
    }

    pub fn is_active(&self) -> bool {
        let (lock, _) = &*self.shared_state;
        !lock.lock().unwrap().active_monitor_ids.is_empty()
    }

    pub fn has_subscriber(&self, monitor_id: &crate::display::MonitorId) -> bool {
        let (lock, _) = &*self.shared_state;
        lock.lock()
            .unwrap()
            .subscribers
            .iter()
            .any(|s| &s.monitor_id == monitor_id)
    }

    pub fn video_path(&self) -> &Path {
        &self.video_path
    }

    pub fn stop(&mut self) {
        {
            let (lock, cvar) = &*self.shared_state;
            let mut state = lock.lock().unwrap();
            state.is_running = false;
            cvar.notify_all();
        }
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }

    fn session_worker_main(
        video_path: PathBuf,
        session_epoch: u64,
        gfx: Arc<presenter::GraphicsDevice>,
        renderer: Arc<Renderer>,
        recovery: Arc<Mutex<DeviceRecoveryCoordinator>>,
        shared: Arc<(Mutex<SharedSessionState>, Condvar)>,
    ) {
        unsafe {
            use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        }

        let mut demuxer = Demuxer::new();
        if let Err(e) = demuxer.open(&video_path) {
            log_with_level(
                LogLevel::Error,
                &format!(
                    "[SharedVideoSession] Demuxer::open failed for {:?}: {}",
                    video_path, e
                ),
            );
            let (lock, cvar) = &*shared;
            let mut state = lock.lock().unwrap();
            while state.is_running {
                let (s, _) = cvar
                    .wait_timeout(state, Duration::from_millis(200))
                    .unwrap();
                state = s;
            }
            unsafe {
                use windows::Win32::Media::MediaFoundation::MFShutdown;
                use windows::Win32::System::Com::CoUninitialize;
                let _ = MFShutdown();
                CoUninitialize();
            }
            return;
        }

        let mut loop_ctrl = LoopController::new(demuxer.frame_count());
        let pacer = match FramePacer::new() {
            Ok(p) => p,
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!("[SharedVideoSession] FramePacer::new failed: {}", e),
                );
                unsafe {
                    use windows::Win32::Media::MediaFoundation::MFShutdown;
                    use windows::Win32::System::Com::CoUninitialize;
                    let _ = MFShutdown();
                    CoUninitialize();
                }
                return;
            }
        };

        let video_w = demuxer.width();
        let video_h = demuxer.height();
        let codec = demuxer.codec().unwrap_or(VideoCodec::H264);

        let native_fps = if demuxer.frame_count() > 0 && demuxer.frames()[0].duration_100ns > 0 {
            (10_000_000.0 / demuxer.frames()[0].duration_100ns as f64).round() as u32
        } else {
            60
        };

        {
            let (lock, _) = &*shared;
            let mut state = lock.lock().unwrap();
            state.native_fps = Some(native_fps);
        }

        let mut decoder = match Decoder::new(gfx.device(), codec, video_w, video_h) {
            Ok(d) => Some(d),
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!(
                        "[SharedVideoSession] Decoder::new failed for {:?}: {}",
                        video_path, e
                    ),
                );
                if presenter::is_device_lost_error(&e)
                    || presenter::is_device_lost_hresult(gfx.device_removed_reason())
                {
                    recovery.lock().unwrap().flag_fault(session_epoch);
                }
                None
            }
        };

        let (lock, cvar) = &*shared;

        loop {
            let (active_subs, speed, target_fps, display_mode, virtual_bounds, is_running) = {
                let mut state = lock.lock().unwrap();
                if !state.is_running {
                    break;
                }

                while state.is_running && state.active_monitor_ids.is_empty() {
                    if recovery.lock().unwrap().is_fault_flagged()
                        || recovery.lock().unwrap().is_exhausted()
                    {
                        break;
                    }
                    let (new_state, _) =
                        cvar.wait_timeout(state, Duration::from_millis(50)).unwrap();
                    state = new_state;
                }

                if !state.is_running {
                    break;
                }

                let active_ids = state.active_monitor_ids.clone();
                let mut subs = Vec::new();
                for sub in &state.subscribers {
                    if active_ids.contains(&sub.monitor_id) {
                        subs.push(sub.clone());
                    }
                }

                (
                    subs,
                    state.speed,
                    state.target_fps,
                    state.display_mode,
                    state.virtual_bounds,
                    state.is_running,
                )
            };

            if !is_running {
                break;
            }

            if active_subs.is_empty() {
                std::thread::sleep(Duration::from_millis(16));
                continue;
            }

            if recovery.lock().unwrap().is_fault_flagged()
                || recovery.lock().unwrap().is_exhausted()
            {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }

            if let Some(ref mut dec) = decoder {
                match demuxer.read_next_annexb_packet(&mut loop_ctrl) {
                    Ok((packet, pts_100ns, duration_100ns, _)) => {
                        let base_interval_100ns = if let Some(tfps) = target_fps {
                            let target_interval = (10_000_000 / tfps.max(1)) as i64;
                            duration_100ns.max(target_interval)
                        } else {
                            duration_100ns
                        };

                        let effective_sleep_100ns =
                            (base_interval_100ns as f64 / (speed as f64).max(0.1)) as i64;

                        pacer.sleep_100ns(effective_sleep_100ns);

                        match dec.feed_and_decode(packet, pts_100ns, duration_100ns) {
                            Ok(Some(frame)) => {
                                for (idx, sub) in active_subs.iter().enumerate() {
                                    let is_primary_presenter = idx == 0;
                                    let vsync = is_primary_presenter;
                                    let do_not_wait = !is_primary_presenter;

                                    let (uv_scale, uv_offset) =
                                        if display_mode == crate::app::config::DisplayMode::Span {
                                            renderer::compute_span_uv(
                                                sub.rect,
                                                virtual_bounds,
                                                dec.width(),
                                                dec.height(),
                                                sub.fit_mode,
                                            )
                                        } else {
                                            renderer::compute_fit_uv(
                                                sub.rect.width().max(1) as u32,
                                                sub.rect.height().max(1) as u32,
                                                dec.width(),
                                                dec.height(),
                                                sub.fit_mode,
                                            )
                                        };

                                    let swap_guard = sub.swap_chain.lock().unwrap();
                                    if let Some(ref swap) = *swap_guard {
                                        if let Some(rtv) = swap.render_target_view() {
                                            let render_res = gfx.with_context(|ctx| {
                                                renderer.render_nv12_with_uv(
                                                    gfx.device(),
                                                    ctx,
                                                    rtv,
                                                    swap.width(),
                                                    swap.height(),
                                                    &frame.texture,
                                                    frame.subresource_index,
                                                    sub.brightness,
                                                    uv_scale,
                                                    uv_offset,
                                                )
                                            });

                                            if let Err(e) = render_res {
                                                log_throttled(
                                                    "session_render_err",
                                                    Duration::from_secs(5),
                                                    LogLevel::Error,
                                                    &format!(
                                                        "[ArcPaper] Shared session render error ({:?}): {}",
                                                        e.code(),
                                                        e
                                                    ),
                                                );
                                                if presenter::is_device_lost_error(&e)
                                                    || presenter::is_device_lost_hresult(
                                                        gfx.device_removed_reason(),
                                                    )
                                                {
                                                    recovery
                                                        .lock()
                                                        .unwrap()
                                                        .flag_fault(session_epoch);
                                                    break;
                                                }
                                            }
                                        }

                                        if let Err(e) = swap.present(vsync, do_not_wait) {
                                            log_throttled(
                                                "session_present_err",
                                                Duration::from_secs(5),
                                                LogLevel::Error,
                                                &format!(
                                                    "[ArcPaper] Shared session present error ({:?}): {}",
                                                    e.code(),
                                                    e
                                                ),
                                            );
                                            if presenter::is_device_lost_error(&e)
                                                || presenter::is_device_lost_hresult(
                                                    gfx.device_removed_reason(),
                                                )
                                            {
                                                recovery.lock().unwrap().flag_fault(session_epoch);
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                log_throttled(
                                    "session_decode_err",
                                    Duration::from_secs(5),
                                    LogLevel::Error,
                                    &format!(
                                        "[SharedVideoSession] feed_and_decode error ({:?}): {}",
                                        e.code(),
                                        e
                                    ),
                                );
                                if presenter::is_device_lost_error(&e)
                                    || presenter::is_device_lost_hresult(
                                        gfx.device_removed_reason(),
                                    )
                                {
                                    recovery.lock().unwrap().flag_fault(session_epoch);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log_throttled(
                            "session_demux_err",
                            Duration::from_secs(5),
                            LogLevel::Warn,
                            &format!("[SharedVideoSession] Demux error: {}", e),
                        );
                        pacer.sleep_duration(Duration::from_millis(16));
                    }
                }
            } else {
                pacer.sleep_duration(Duration::from_millis(100));
            }
        }

        drop(decoder);

        unsafe {
            use windows::Win32::Media::MediaFoundation::MFShutdown;
            use windows::Win32::System::Com::CoUninitialize;

            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

#[cfg(windows)]
impl Drop for SharedVideoSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
struct MonitorSlot {
    info: crate::display::MonitorInfo,
    child_host: presenter::ChildHostWindow,
    swap_chain: Arc<Mutex<Option<presenter::MonitorSwapChain>>>,
    last_mapped_pos: (i32, i32, i32, i32),
}

/// Central multi-monitor wallpaper coordinator managing display topology,
/// child host windows, shared GPU decoding sessions, and device-wide fault recovery.
#[cfg(windows)]
pub struct WallpaperManager {
    workerw_hwnd: HWND,
    graphics: Option<Arc<presenter::GraphicsDevice>>,
    renderer: Option<Arc<Renderer>>,
    monitors: BTreeMap<crate::display::MonitorId, MonitorSlot>,
    sessions: BTreeMap<PathBuf, SharedVideoSession>,
    recovery: Arc<Mutex<DeviceRecoveryCoordinator>>,
    is_globally_hibernated: bool,
    next_recovery_retry_at: Option<Instant>,
    is_recovering: bool,
    cached_monitors: Vec<crate::display::MonitorInfo>,
    cached_config: crate::app::config::AppConfig,
    cached_paused_by_monitor: BTreeMap<crate::display::MonitorId, bool>,
    cached_wallpapers_dir: PathBuf,
}

#[cfg(windows)]
impl WallpaperManager {
    /// Instantiates a new WallpaperManager bound to the desktop WorkerW parent window.
    pub fn new(workerw_hwnd: HWND) -> Result<Self, String> {
        let recovery = Arc::new(Mutex::new(DeviceRecoveryCoordinator::new()));

        let (graphics, renderer) = match presenter::GraphicsDevice::new() {
            Ok(gfx) => {
                let gfx_arc = Arc::new(gfx);
                match Renderer::new(gfx_arc.device()) {
                    Ok(rend) => (Some(gfx_arc), Some(Arc::new(rend))),
                    Err(e) => {
                        log_with_level(
                            LogLevel::Error,
                            &format!("[WallpaperManager] Initial Renderer::new failed: {}", e),
                        );
                        recovery.lock().unwrap().flag_fault(0);
                        (Some(gfx_arc), None)
                    }
                }
            }
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!(
                        "[WallpaperManager] Initial GraphicsDevice::new failed: {}",
                        e
                    ),
                );
                recovery.lock().unwrap().flag_fault(0);
                (None, None)
            }
        };

        Ok(Self {
            workerw_hwnd,
            graphics,
            renderer,
            monitors: BTreeMap::new(),
            sessions: BTreeMap::new(),
            recovery,
            is_globally_hibernated: false,
            next_recovery_retry_at: None,
            is_recovering: false,
            cached_monitors: Vec::new(),
            cached_config: crate::app::config::AppConfig::default(),
            cached_paused_by_monitor: BTreeMap::new(),
            cached_wallpapers_dir: PathBuf::new(),
        })
    }

    /// Synchronizes monitor topology, child window positions, and decoding sessions with the current configuration.
    pub fn apply_topology_and_config(
        &mut self,
        workerw_hwnd: HWND,
        monitors: &[crate::display::MonitorInfo],
        config: &crate::app::config::AppConfig,
        paused_by_monitor: &BTreeMap<crate::display::MonitorId, bool>,
        wallpapers_dir: &Path,
    ) {
        self.workerw_hwnd = workerw_hwnd;
        self.cached_monitors = monitors.to_vec();
        self.cached_config = config.clone();
        self.cached_paused_by_monitor = paused_by_monitor.clone();
        self.cached_wallpapers_dir = wallpapers_dir.to_path_buf();

        if self.recovery.lock().unwrap().is_exhausted() || self.graphics.is_none() {
            self.recovery.lock().unwrap().reset_on_video_load();
            self.is_recovering = false;
            self.next_recovery_retry_at = None;
        }

        let current_ids: HashSet<crate::display::MonitorId> =
            monitors.iter().map(|m| m.id.clone()).collect();

        // 1. Drop removed monitors (drops ChildHostWindow on main thread)
        self.monitors.retain(|id, _| current_ids.contains(id));

        // 2. Add or update monitors
        for mon in monitors {
            let (x, y, w, h) = crate::workerw::map_monitor_rect_to_workerw(workerw_hwnd, mon.rect);
            let cur_pos = (x, y, w, h);

            if let Some(slot) = self.monitors.get_mut(&mon.id) {
                slot.info = mon.clone();
                if slot.last_mapped_pos != cur_pos {
                    slot.child_host.reposition(x, y, w, h);
                    let (_, _, old_w, old_h) = slot.last_mapped_pos;
                    if (old_w != w || old_h != h)
                        && let Some(ref gfx) = self.graphics
                    {
                        let mut swap_guard = slot.swap_chain.lock().unwrap();
                        if let Some(ref mut swap) = *swap_guard
                            && let Err(e) =
                                swap.resize(gfx.device(), w.max(1) as u32, h.max(1) as u32)
                        {
                            log_throttled(
                                "swap_resize_err",
                                Duration::from_secs(5),
                                LogLevel::Error,
                                &format!("[WallpaperManager] swap.resize failed: {}", e),
                            );
                            if presenter::is_device_lost_error(&e)
                                || presenter::is_device_lost_hresult(gfx.device_removed_reason())
                            {
                                let epoch = self.recovery.lock().unwrap().epoch();
                                self.recovery.lock().unwrap().flag_fault(epoch);
                            }
                        }
                    }
                    slot.last_mapped_pos = cur_pos;
                }
            } else {
                let child = match presenter::ChildHostWindow::create(workerw_hwnd, x, y, w, h) {
                    Ok(c) => c,
                    Err(e) => {
                        log_with_level(
                            LogLevel::Error,
                            &format!("[WallpaperManager] ChildHostWindow::create failed: {}", e),
                        );
                        continue;
                    }
                };

                let swap = if let Some(ref gfx) = self.graphics {
                    match presenter::MonitorSwapChain::new(
                        gfx,
                        child.hwnd(),
                        w.max(1) as u32,
                        h.max(1) as u32,
                    ) {
                        Ok(sc) => Some(sc),
                        Err(e) => {
                            log_with_level(
                                LogLevel::Error,
                                &format!("[WallpaperManager] MonitorSwapChain::new failed: {}", e),
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                self.monitors.insert(
                    mon.id.clone(),
                    MonitorSlot {
                        info: mon.clone(),
                        child_host: child,
                        swap_chain: Arc::new(Mutex::new(swap)),
                        last_mapped_pos: cur_pos,
                    },
                );
            }
        }

        // 3. Build MonitorPlaybackSpec for each monitor
        let specs: Vec<MonitorPlaybackSpec> = monitors
            .iter()
            .map(|mon| {
                let res_cfg = config.resolve_for_monitor(&mon.id);
                let video_path = res_cfg.active_video.and_then(|v| {
                    if v.trim().is_empty() {
                        None
                    } else {
                        let p = PathBuf::from(&v);
                        Some(if p.is_absolute() {
                            p
                        } else {
                            wallpapers_dir.join(p)
                        })
                    }
                });

                let is_paused = res_cfg.is_manually_paused
                    || paused_by_monitor.get(&mon.id).copied().unwrap_or(false);

                let brightness = (res_cfg.brightness as f32 / 100.0).clamp(0.0, 1.0);
                let fit_mode = ScreenFit::from_str_mode(&res_cfg.fit_mode);

                MonitorPlaybackSpec {
                    monitor_id: mon.id.clone(),
                    video_path,
                    is_paused,
                    brightness,
                    fit_mode,
                    speed: res_cfg.speed,
                    target_fps: res_cfg.target_fps,
                    is_primary: mon.is_primary,
                    rect: mon.rect,
                }
            })
            .collect();

        // 4. Session Plan
        let plan = build_session_plan(&specs);

        // 5. Global hibernation guard
        if plan.should_trim_global_memory() {
            if !self.is_globally_hibernated {
                if let Some(ref gfx) = self.graphics {
                    AutoPause::enter_global_hibernation(gfx);
                }
                self.is_globally_hibernated = true;
            }
        } else {
            self.is_globally_hibernated = false;
        }

        // 6. Reconcile SharedVideoSessions
        let virtual_bounds = crate::display::compute_virtual_bounds(monitors);
        let active_paths: HashSet<PathBuf> =
            plan.sessions.iter().map(|s| s.video_path.clone()).collect();

        // Stop and remove sessions no longer in plan (dropping the session invokes Drop -> stop())
        self.sessions.retain(|path, _| active_paths.contains(path));

        // Update existing sessions or spawn new ones
        for assignment in &plan.sessions {
            let mut subscribers = Vec::new();
            for sub_id in &assignment.subscriber_ids {
                if let Some(slot) = self.monitors.get(sub_id) {
                    let spec = specs.iter().find(|s| &s.monitor_id == sub_id);
                    let fit_mode = spec.map(|s| s.fit_mode).unwrap_or_default();
                    let brightness = spec.map(|s| s.brightness).unwrap_or(1.0);
                    subscribers.push(SubscriberRenderInfo {
                        monitor_id: sub_id.clone(),
                        swap_chain: Arc::clone(&slot.swap_chain),
                        rect: slot.info.rect,
                        fit_mode,
                        brightness,
                        is_primary: slot.info.is_primary,
                    });
                }
            }

            if let Some(existing) = self.sessions.get_mut(&assignment.video_path) {
                existing.update_subscribers(
                    subscribers,
                    assignment.active_subscriber_ids.clone(),
                    assignment.effective_speed,
                    assignment.effective_target_fps,
                    config.display_mode,
                    virtual_bounds,
                );
            } else if let (Some(gfx), Some(rend)) = (&self.graphics, &self.renderer) {
                let epoch = self.recovery.lock().unwrap().epoch();
                match SharedVideoSession::new(
                    assignment.video_path.clone(),
                    epoch,
                    Arc::clone(gfx),
                    Arc::clone(rend),
                    Arc::clone(&self.recovery),
                    subscribers,
                    assignment.active_subscriber_ids.clone(),
                    assignment.effective_speed,
                    assignment.effective_target_fps,
                    config.display_mode,
                    virtual_bounds,
                ) {
                    Ok(session) => {
                        self.sessions.insert(assignment.video_path.clone(), session);
                    }
                    Err(e) => {
                        log_with_level(
                            LogLevel::Error,
                            &format!(
                                "[WallpaperManager] Failed creating SharedVideoSession for {:?}: {}",
                                assignment.video_path, e
                            ),
                        );
                    }
                }
            }
        }
    }

    /// Reparents all managed child host windows to a newly spawned WorkerW window (e.g. after explorer.exe restarts).
    pub fn reparent_to_workerw(
        &mut self,
        new_workerw: HWND,
        monitors: &[crate::display::MonitorInfo],
    ) {
        self.workerw_hwnd = new_workerw;
        self.cached_monitors = monitors.to_vec();
        for mon in monitors {
            if let Some(slot) = self.monitors.get_mut(&mon.id) {
                let (x, y, w, h) =
                    crate::workerw::map_monitor_rect_to_workerw(new_workerw, mon.rect);
                slot.child_host.reparent(new_workerw, x, y, w, h);
                slot.last_mapped_pos = (x, y, w, h);
            }
        }
    }

    /// Updates playback pause states across all monitors without tearing down swapchains.
    pub fn update_pause_states(
        &mut self,
        config: &crate::app::config::AppConfig,
        paused_by_monitor: &BTreeMap<crate::display::MonitorId, bool>,
    ) {
        self.cached_config = config.clone();
        self.cached_paused_by_monitor = paused_by_monitor.clone();

        let specs: Vec<MonitorPlaybackSpec> = self
            .cached_monitors
            .iter()
            .map(|mon| {
                let res_cfg = config.resolve_for_monitor(&mon.id);
                let video_path = res_cfg.active_video.and_then(|v| {
                    if v.trim().is_empty() {
                        None
                    } else {
                        let p = PathBuf::from(&v);
                        Some(if p.is_absolute() {
                            p
                        } else {
                            self.cached_wallpapers_dir.join(p)
                        })
                    }
                });

                let is_paused = res_cfg.is_manually_paused
                    || paused_by_monitor.get(&mon.id).copied().unwrap_or(false);

                let brightness = (res_cfg.brightness as f32 / 100.0).clamp(0.0, 1.0);
                let fit_mode = ScreenFit::from_str_mode(&res_cfg.fit_mode);

                MonitorPlaybackSpec {
                    monitor_id: mon.id.clone(),
                    video_path,
                    is_paused,
                    brightness,
                    fit_mode,
                    speed: res_cfg.speed,
                    target_fps: res_cfg.target_fps,
                    is_primary: mon.is_primary,
                    rect: mon.rect,
                }
            })
            .collect();

        let plan = build_session_plan(&specs);

        if plan.should_trim_global_memory() {
            if !self.is_globally_hibernated {
                if let Some(ref gfx) = self.graphics {
                    AutoPause::enter_global_hibernation(gfx);
                }
                self.is_globally_hibernated = true;
            }
        } else {
            self.is_globally_hibernated = false;
        }

        let virtual_bounds = crate::display::compute_virtual_bounds(&self.cached_monitors);

        for assignment in &plan.sessions {
            if let Some(session) = self.sessions.get_mut(&assignment.video_path) {
                let mut subscribers = Vec::new();
                for sub_id in &assignment.subscriber_ids {
                    if let Some(slot) = self.monitors.get(sub_id) {
                        let spec = specs.iter().find(|s| &s.monitor_id == sub_id);
                        let fit_mode = spec.map(|s| s.fit_mode).unwrap_or_default();
                        let brightness = spec.map(|s| s.brightness).unwrap_or(1.0);
                        subscribers.push(SubscriberRenderInfo {
                            monitor_id: sub_id.clone(),
                            swap_chain: Arc::clone(&slot.swap_chain),
                            rect: slot.info.rect,
                            fit_mode,
                            brightness,
                            is_primary: slot.info.is_primary,
                        });
                    }
                }

                session.update_subscribers(
                    subscribers,
                    assignment.active_subscriber_ids.clone(),
                    assignment.effective_speed,
                    assignment.effective_target_fps,
                    config.display_mode,
                    virtual_bounds,
                );
            }
        }
    }

    /// Returns the native frame rate of the video currently playing on the specified monitor.
    pub fn native_fps_for_monitor(&self, monitor_id: &crate::display::MonitorId) -> Option<u32> {
        for session in self.sessions.values() {
            if session.has_subscriber(monitor_id) {
                return session.native_fps();
            }
        }
        None
    }

    /// Performs non-blocking recovery polling, rebuilding D3D11 resources if a GPU device-lost was flagged.
    pub fn poll_recovery(&mut self) {
        let fault_flagged = self.recovery.lock().unwrap().is_fault_flagged();

        if fault_flagged {
            let backoff = self.recovery.lock().unwrap().begin_recovery_step();

            for (_, mut session) in std::mem::take(&mut self.sessions) {
                session.stop();
            }

            self.renderer = None;

            for slot in self.monitors.values() {
                *slot.swap_chain.lock().unwrap() = None;
            }

            self.graphics = None;

            match backoff {
                Some(duration) => {
                    log_with_level(
                        LogLevel::Warn,
                        &format!(
                            "[WallpaperManager] Device recovery step triggered; waiting {:?}...",
                            duration
                        ),
                    );
                    self.next_recovery_retry_at = Some(Instant::now() + duration);
                    self.is_recovering = true;
                }
                None => {
                    log_with_level(
                        LogLevel::Error,
                        "[WallpaperManager] Device recovery exhausted after max attempts.",
                    );
                    self.is_recovering = false;
                    self.next_recovery_retry_at = None;
                }
            }
            return;
        }

        if self.is_recovering {
            if let Some(retry_at) = self.next_recovery_retry_at
                && Instant::now() < retry_at
            {
                return;
            }
            self.next_recovery_retry_at = None;

            log_with_level(
                LogLevel::Info,
                "[WallpaperManager] Attempting to rebuild Direct3D 11 device and swapchains...",
            );

            let new_gfx = match presenter::GraphicsDevice::new() {
                Ok(g) => Arc::new(g),
                Err(e) => {
                    log_with_level(
                        LogLevel::Error,
                        &format!("[WallpaperManager] GraphicsDevice::new failed: {}", e),
                    );
                    let epoch = self.recovery.lock().unwrap().epoch();
                    self.recovery.lock().unwrap().flag_fault(epoch);
                    return;
                }
            };

            let new_renderer = match Renderer::new(new_gfx.device()) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    log_with_level(
                        LogLevel::Error,
                        &format!("[WallpaperManager] Renderer::new failed: {}", e),
                    );
                    let epoch = self.recovery.lock().unwrap().epoch();
                    self.recovery.lock().unwrap().flag_fault(epoch);
                    return;
                }
            };

            let mut all_sc_ok = true;
            for slot in self.monitors.values() {
                let (_, _, w, h) = slot.last_mapped_pos;
                match presenter::MonitorSwapChain::new(
                    &new_gfx,
                    slot.child_host.hwnd(),
                    w.max(1) as u32,
                    h.max(1) as u32,
                ) {
                    Ok(sc) => {
                        *slot.swap_chain.lock().unwrap() = Some(sc);
                    }
                    Err(e) => {
                        log_with_level(
                            LogLevel::Error,
                            &format!("[WallpaperManager] MonitorSwapChain::new failed: {}", e),
                        );
                        all_sc_ok = false;
                        break;
                    }
                }
            }

            if !all_sc_ok {
                let epoch = self.recovery.lock().unwrap().epoch();
                self.recovery.lock().unwrap().flag_fault(epoch);
                return;
            }

            self.graphics = Some(new_gfx);
            self.renderer = Some(new_renderer);
            self.is_recovering = false;
            self.recovery.lock().unwrap().mark_healthy();

            log_with_level(
                LogLevel::Info,
                "[WallpaperManager] Pipeline recovery successful! Restarting wallpaper sessions.",
            );

            let monitors = self.cached_monitors.clone();
            let config = self.cached_config.clone();
            let paused_by_mon = self.cached_paused_by_monitor.clone();
            let wallpapers_dir = self.cached_wallpapers_dir.clone();
            let workerw = self.workerw_hwnd;

            self.apply_topology_and_config(
                workerw,
                &monitors,
                &config,
                &paused_by_mon,
                &wallpapers_dir,
            );
        }
    }

    /// Returns the number of sessions with active (non-paused) subscribers.
    pub fn active_session_count(&self) -> usize {
        self.sessions.values().filter(|s| s.is_active()).count()
    }

    /// Returns the number of managed monitor slots.
    pub fn monitor_count(&self) -> usize {
        self.monitors.len()
    }

    /// Returns whether the manager is currently waiting on a recovery backoff timer.
    pub fn is_recovering(&self) -> bool {
        self.is_recovering
    }

    /// Returns whether recovery attempts have exceeded the maximum threshold.
    pub fn is_recovery_exhausted(&self) -> bool {
        self.recovery.lock().unwrap().is_exhausted()
    }

    /// Returns the number of consecutive recovery attempts.
    pub fn recovery_attempts(&self) -> u32 {
        self.recovery.lock().unwrap().attempts()
    }
}

#[cfg(windows)]
impl Drop for WallpaperManager {
    fn drop(&mut self) {
        for (_, mut session) in std::mem::take(&mut self.sessions) {
            session.stop();
        }
        self.renderer = None;
        for slot in self.monitors.values() {
            *slot.swap_chain.lock().unwrap() = None;
        }
        self.graphics = None;
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

    #[test]
    fn test_session_plan_shares_identical_video_and_splits_diverging() {
        let mon_a = crate::display::MonitorId::from("MON-A");
        let mon_b = crate::display::MonitorId::from("MON-B");
        let video_1 = std::path::PathBuf::from("wallpaper_1.mp4");
        let video_2 = std::path::PathBuf::from("wallpaper_2.mp4");

        let specs_shared = vec![
            MonitorPlaybackSpec {
                monitor_id: mon_a.clone(),
                video_path: Some(video_1.clone()),
                is_paused: false,
                brightness: 1.0,
                fit_mode: ScreenFit::Cover,
                speed: 1.0,
                target_fps: Some(60),
                is_primary: true,
                rect: crate::display::DisplayRect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
            },
            MonitorPlaybackSpec {
                monitor_id: mon_b.clone(),
                video_path: Some(video_1.clone()),
                is_paused: false,
                brightness: 0.8,
                fit_mode: ScreenFit::Contain,
                speed: 1.0,
                target_fps: Some(60),
                is_primary: false,
                rect: crate::display::DisplayRect {
                    left: 1920,
                    top: 0,
                    right: 3840,
                    bottom: 1080,
                },
            },
        ];

        let plan_shared = build_session_plan(&specs_shared);
        assert_eq!(plan_shared.sessions.len(), 1);
        assert_eq!(plan_shared.total_active_monitors, 2);
        assert_eq!(plan_shared.sessions[0].video_path, video_1);
        assert_eq!(
            plan_shared.sessions[0].subscriber_ids,
            vec![mon_a.clone(), mon_b.clone()]
        );
        assert_eq!(
            plan_shared.sessions[0].active_subscriber_ids,
            vec![mon_a.clone(), mon_b.clone()]
        );
        assert_eq!(plan_shared.active_subscribers_for(&video_1), 2);

        let specs_split = vec![
            MonitorPlaybackSpec {
                monitor_id: mon_a.clone(),
                video_path: Some(video_1.clone()),
                is_paused: false,
                brightness: 1.0,
                fit_mode: ScreenFit::Cover,
                speed: 1.0,
                target_fps: Some(60),
                is_primary: true,
                rect: crate::display::DisplayRect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
            },
            MonitorPlaybackSpec {
                monitor_id: mon_b.clone(),
                video_path: Some(video_2.clone()),
                is_paused: false,
                brightness: 0.8,
                fit_mode: ScreenFit::Contain,
                speed: 1.2,
                target_fps: Some(30),
                is_primary: false,
                rect: crate::display::DisplayRect {
                    left: 1920,
                    top: 0,
                    right: 3840,
                    bottom: 1080,
                },
            },
        ];

        let plan_split = build_session_plan(&specs_split);
        assert_eq!(plan_split.sessions.len(), 2);
        assert_eq!(plan_split.total_active_monitors, 2);
        assert_eq!(plan_split.active_subscribers_for(&video_1), 1);
        assert_eq!(plan_split.active_subscribers_for(&video_2), 1);
    }

    #[test]
    fn test_session_plan_partial_vs_full_hibernation_trim_guard() {
        let mon_a = crate::display::MonitorId::from("MON-A");
        let mon_b = crate::display::MonitorId::from("MON-B");
        let video = std::path::PathBuf::from("wallpaper.mp4");

        let specs_partial = vec![
            MonitorPlaybackSpec {
                monitor_id: mon_a.clone(),
                video_path: Some(video.clone()),
                is_paused: true,
                brightness: 1.0,
                fit_mode: ScreenFit::Cover,
                speed: 1.0,
                target_fps: None,
                is_primary: true,
                rect: crate::display::DisplayRect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
            },
            MonitorPlaybackSpec {
                monitor_id: mon_b.clone(),
                video_path: Some(video.clone()),
                is_paused: false,
                brightness: 1.0,
                fit_mode: ScreenFit::Cover,
                speed: 1.0,
                target_fps: None,
                is_primary: false,
                rect: crate::display::DisplayRect {
                    left: 1920,
                    top: 0,
                    right: 3840,
                    bottom: 1080,
                },
            },
        ];

        let plan_partial = build_session_plan(&specs_partial);
        assert_eq!(plan_partial.total_active_monitors, 1);
        assert_eq!(plan_partial.active_subscribers_for(&video), 1);
        assert_eq!(plan_partial.sessions[0].subscriber_ids.len(), 2);
        assert_eq!(
            plan_partial.sessions[0].active_subscriber_ids,
            vec![mon_b.clone()]
        );
        assert!(!plan_partial.should_trim_global_memory());

        let specs_full = vec![
            MonitorPlaybackSpec {
                monitor_id: mon_a.clone(),
                video_path: Some(video.clone()),
                is_paused: true,
                brightness: 1.0,
                fit_mode: ScreenFit::Cover,
                speed: 1.0,
                target_fps: None,
                is_primary: true,
                rect: crate::display::DisplayRect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
            },
            MonitorPlaybackSpec {
                monitor_id: mon_b.clone(),
                video_path: Some(video.clone()),
                is_paused: true,
                brightness: 1.0,
                fit_mode: ScreenFit::Cover,
                speed: 1.0,
                target_fps: None,
                is_primary: false,
                rect: crate::display::DisplayRect {
                    left: 1920,
                    top: 0,
                    right: 3840,
                    bottom: 1080,
                },
            },
        ];

        let plan_full = build_session_plan(&specs_full);
        assert_eq!(plan_full.total_active_monitors, 0);
        assert_eq!(plan_full.active_subscribers_for(&video), 0);
        assert_eq!(plan_full.sessions[0].subscriber_ids.len(), 2);
        assert!(plan_full.sessions[0].active_subscriber_ids.is_empty());
        assert!(plan_full.should_trim_global_memory());
    }

    #[test]
    fn test_device_recovery_coordinator_epoch_isolation_and_exhaustion() {
        let mut coord = DeviceRecoveryCoordinator::new();
        assert_eq!(coord.epoch(), 0);
        assert!(!coord.is_fault_flagged());
        assert!(!coord.is_exhausted());
        assert_eq!(coord.attempts(), 0);

        assert!(coord.flag_fault(0));
        assert!(coord.is_fault_flagged());

        assert!(!coord.flag_fault(0));
        assert!(!coord.flag_fault(99));

        let step1 = coord.begin_recovery_step();
        assert_eq!(step1, Some(Duration::from_millis(150)));
        assert_eq!(coord.epoch(), 1);
        assert!(!coord.is_fault_flagged());
        assert_eq!(coord.attempts(), 1);

        assert!(!coord.flag_fault(0));

        assert!(coord.flag_fault(1));
        let step2 = coord.begin_recovery_step();
        assert_eq!(step2, Some(Duration::from_millis(400)));
        assert_eq!(coord.epoch(), 2);
        assert_eq!(coord.attempts(), 2);

        assert!(coord.flag_fault(2));
        let step3 = coord.begin_recovery_step();
        assert_eq!(step3, Some(Duration::from_millis(800)));
        assert_eq!(coord.epoch(), 3);
        assert_eq!(coord.attempts(), 3);

        assert!(coord.flag_fault(3));
        let step4 = coord.begin_recovery_step();
        assert_eq!(step4, None);
        assert!(coord.is_exhausted());

        assert!(!coord.flag_fault(coord.epoch()));

        coord.reset_on_video_load();
        assert!(!coord.is_exhausted());
        assert_eq!(coord.attempts(), 0);
        assert!(coord.is_fault_flagged());

        let _ = coord.begin_recovery_step();
        coord.mark_healthy();
        assert!(!coord.is_fault_flagged());
        assert_eq!(coord.attempts(), 0);
        assert!(!coord.is_exhausted());
    }

    #[cfg(windows)]
    #[test]
    fn test_wallpaper_manager_lifecycle_and_topology_dispatch() {
        let mut mgr = match WallpaperManager::new(HWND::default()) {
            Ok(m) => m,
            Err(e) => panic!("WallpaperManager::new failed: {e}"),
        };

        assert_eq!(mgr.monitor_count(), 0);
        assert_eq!(mgr.active_session_count(), 0);
        assert!(!mgr.is_recovering());

        let mon1 = crate::display::MonitorInfo {
            id: crate::display::MonitorId::from("TEST-MON-1"),
            friendly_name: "Test Monitor 1".to_string(),
            gdi_name: r"\\.\DISPLAY1".to_string(),
            rect: crate::display::DisplayRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: true,
        };

        let mon2 = crate::display::MonitorInfo {
            id: crate::display::MonitorId::from("TEST-MON-2"),
            friendly_name: "Test Monitor 2".to_string(),
            gdi_name: r"\\.\DISPLAY2".to_string(),
            rect: crate::display::DisplayRect {
                left: 1920,
                top: 0,
                right: 3840,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };

        let monitors = vec![mon1.clone(), mon2.clone()];
        let config = crate::app::config::AppConfig {
            active_video: Some("sample_h264.mp4".to_string()),
            ..Default::default()
        };
        let paused_by_mon = std::collections::BTreeMap::new();
        let assets_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("assets");

        mgr.apply_topology_and_config(
            HWND::default(),
            &monitors,
            &config,
            &paused_by_mon,
            &assets_dir,
        );

        assert_eq!(mgr.monitor_count(), 2);
        mgr.poll_recovery();
        mgr.reparent_to_workerw(HWND::default(), &monitors);

        // Pause both monitors
        let mut paused_map = std::collections::BTreeMap::new();
        paused_map.insert(mon1.id.clone(), true);
        paused_map.insert(mon2.id.clone(), true);
        mgr.update_pause_states(&config, &paused_map);
        assert_eq!(mgr.active_session_count(), 0);
    }
}
