//! ArcPlayer: High-performance, zero-copy, low-memory native Windows video engine.

pub mod autopause;
pub mod decoder;
pub mod demux;
pub mod loop_ctrl;
pub mod presenter;
pub mod renderer;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

pub use autopause::{AutoPause, FramePacer};
pub use decoder::{DecodedFrame, Decoder};
pub use demux::{Demuxer, VideoCodec};
pub use loop_ctrl::LoopController;
pub use presenter::Presenter;
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

/// Main ArcPlayer coordinator orchestrating playback via a dedicated background rendering thread.
pub struct ArcPlayer {
    state: Arc<(Mutex<SharedState>, Condvar)>,
    thread_handle: Option<JoinHandle<()>>,
}

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
            use windows::Win32::Media::MediaFoundation::{MFStartup, MF_VERSION, MFSTARTUP_FULL};
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        }

        let (lock, cvar) = &*shared;

        // 1. Initialize Direct3D 11 Presenter, Renderer, and FramePacer
        let mut presenter = match Presenter::new(hwnd, width, height) {
            Ok(p) => p,
            Err(_) => return,
        };

        let renderer = match Renderer::new(presenter.device()) {
            Ok(r) => r,
            Err(_) => return,
        };

        let pacer = match FramePacer::new() {
            Ok(p) => p,
            Err(_) => return,
        };

        let mut loaded_path: Option<PathBuf> = None;
        let mut demuxer: Option<Demuxer> = None;
        let mut loop_ctrl: Option<LoopController> = None;
        let mut decoder: Option<Decoder> = None;
        let mut was_paused = false;

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

            // Handle window resize requests
            if let Some((new_w, new_h)) = resize_req {
                let _ = presenter.resize(new_w, new_h);
            }

            // Handle loading a new video file
            if video_to_load != loaded_path {
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

                            let dec = Decoder::new(presenter.device(), codec, video_w, video_h).ok();

                            let mut state = lock.lock().unwrap();
                            state.native_fps = Some(native_fps);

                            loop_ctrl = Some(LoopController::new(frame_count));
                            decoder = dec;
                            demuxer = Some(d);

                            if first_video_load_time.is_none() {
                                first_video_load_time = Some(Instant::now());
                            }
                        }
                        Err(_) => {
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
            if is_paused {
                if !was_paused {
                    AutoPause::enter_hibernation(&presenter);
                    was_paused = true;
                }

                let mut state = lock.lock().unwrap();
                while state.is_paused
                    && state.is_running
                    && state.video_path == loaded_path
                    && state.resize_req.is_none()
                {
                    let (new_state, _) = cvar
                        .wait_timeout(state, Duration::from_millis(50))
                        .unwrap();
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
            if let (Some(demux), Some(lctrl), Some(dec)) =
                (&mut demuxer, &mut loop_ctrl, &mut decoder)
            {
                match demux.read_next_annexb_packet(lctrl) {
                    Ok((packet, pts_100ns, duration_100ns, _)) => {
                        // Calculate target frame pacing interval
                        let base_interval_100ns = if let Some(target_fps) = target_fps {
                            let target_interval = (10_000_000 / target_fps.max(1)) as i64;
                            duration_100ns.max(target_interval)
                        } else {
                            duration_100ns
                        };

                        let effective_sleep_100ns =
                            (base_interval_100ns as f64 / (speed as f64).max(0.1)) as i64;

                        // High-precision sleep using waitable timer
                        pacer.sleep_100ns(effective_sleep_100ns);

                        // Feed bitstream and present decoded frame
                        if let Ok(Some(frame)) = dec.feed_and_decode(packet, pts_100ns, duration_100ns) {
                            let _ = renderer.render_nv12_frame(
                                &presenter,
                                &frame.texture,
                                frame.subresource_index,
                                demux.width(),
                                demux.height(),
                                brightness,
                                fit_mode,
                            );
                            let _ = presenter.present(true);
                        }
                    }
                    Err(_) => {
                        pacer.sleep_duration(Duration::from_millis(16));
                    }
                }
            } else {
                // No active video stream: wait idle
                pacer.sleep_duration(Duration::from_millis(33));
            }
        }

        // Cleanup upon thread exit
        AutoPause::enter_hibernation(&presenter);
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
}
