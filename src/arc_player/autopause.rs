//! Autopause management and high-precision frame pacing via Win32 waitable timers.

use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, GetCurrentProcess, INFINITE,
    SetProcessWorkingSetSize, SetWaitableTimer, TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::core::{Error, Result};

use super::presenter::Presenter;

/// High-precision timer wrapper utilizing Windows high-resolution waitable timers (~0.5ms resolution).
pub struct FramePacer {
    timer: HANDLE,
}

// Safety: The Win32 waitable timer handle can be safely sent across threads.
unsafe impl Send for FramePacer {}
unsafe impl Sync for FramePacer {}

impl FramePacer {
    /// Instantiates a new high-resolution waitable timer, falling back to standard precision if needed.
    pub fn new() -> Result<Self> {
        let handle = unsafe {
            CreateWaitableTimerExW(
                None,
                windows::core::PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        };

        let timer = match handle {
            Ok(h) if !h.is_invalid() => h,
            _ => unsafe {
                CreateWaitableTimerExW(None, windows::core::PCWSTR::null(), 0, TIMER_ALL_ACCESS.0)?
            },
        };

        if timer.is_invalid() {
            return Err(Error::from_hresult(E_FAIL));
        }

        Ok(Self { timer })
    }

    /// Sleeps for the specified interval in 100-nanosecond units.
    pub fn sleep_100ns(&self, duration_100ns: i64) {
        if duration_100ns <= 0 {
            return;
        }

        // In Win32 SetWaitableTimer, negative due_time indicates relative time
        let due_time = -duration_100ns;
        unsafe {
            if SetWaitableTimer(self.timer, &due_time, 0, None, None, false).is_ok() {
                let _ = WaitForSingleObject(self.timer, INFINITE);
            } else {
                std::thread::sleep(Duration::from_nanos((duration_100ns as u64) * 100));
            }
        }
    }

    /// Sleeps for the specified duration using the high-resolution waitable timer.
    pub fn sleep_duration(&self, duration: Duration) {
        let dur_100ns = (duration.as_nanos() / 100) as i64;
        self.sleep_100ns(dur_100ns);
    }
}

impl Drop for FramePacer {
    fn drop(&mut self) {
        if !self.timer.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.timer);
            }
        }
    }
}

/// Manages hibernation and fullscreen auto-pause states.
pub struct AutoPause;

impl AutoPause {
    /// Performs GPU resource trimming and releases process working set pages.
    ///
    /// Must be called strictly ONCE upon transitioning into the paused/covered state.
    pub fn enter_hibernation(presenter: &Presenter) {
        // 1. Flush Direct3D 11 pipeline and trim driver surface allocation caches
        presenter.trim_gpu();

        // 2. Trim process working set pages to release heap fragments
        Self::trim_working_set();
    }

    /// Trims the shared GPU device and process working set.
    /// Caller must invoke this strictly once when ALL monitors transition into paused/hibernated state.
    pub fn enter_global_hibernation(gfx: &super::presenter::GraphicsDevice) {
        gfx.trim_gpu();
        Self::trim_working_set();
    }

    /// Trims the process working set, commanding the Windows kernel to page out unused private bytes.
    pub fn trim_working_set() {
        unsafe {
            let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        }
    }
}

/// Computes the effective target FPS based on power status and adaptive throttling policy (FR-306).
///
/// Hysteresis behavior:
/// - Engages throttling to 30 FPS when on DC power and battery is at or below `low_threshold`%.
/// - Releases throttling back to `base_target_fps` when on AC power or battery reaches `low_threshold + 5`%.
/// - In the hysteresis deadband (`low_threshold < battery < low_threshold + 5`), retains `is_currently_throttled`.
pub fn compute_adaptive_target_fps(
    base_target_fps: Option<u32>,
    adaptive_enabled: bool,
    battery_percent: Option<u8>,
    is_on_ac: bool,
    is_currently_throttled: bool,
    low_threshold: u32,
) -> (Option<u32>, bool) {
    if !adaptive_enabled || is_on_ac {
        return (base_target_fps, false);
    }

    let Some(percent) = battery_percent else {
        return (base_target_fps, false);
    };

    let percent = percent as u32;
    let release_threshold = low_threshold.saturating_add(5);

    let is_throttled = if is_currently_throttled {
        percent < release_threshold
    } else {
        percent <= low_threshold
    };

    if is_throttled {
        (Some(30), true)
    } else {
        (base_target_fps, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_high_res_pacer_timing() {
        let pacer = FramePacer::new().expect("Failed creating FramePacer");
        let start = Instant::now();
        // 10ms = 100_000 in 100ns units
        pacer.sleep_100ns(100_000);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() >= 8 && elapsed.as_millis() <= 25,
            "Timer sleep was outside expected bounds: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_trim_working_set() {
        AutoPause::trim_working_set();
    }

    #[test]
    fn test_adaptive_fps_hysteresis_no_flapping() {
        let base_fps = Some(60);
        let low_thresh = 20;

        // 1. Adaptive disabled -> never throttled
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, false, Some(10), false, false, low_thresh);
        assert_eq!(fps, base_fps);
        assert!(!throttled);

        // 2. On AC power -> never throttled even with low battery
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(10), true, true, low_thresh);
        assert_eq!(fps, base_fps);
        assert!(!throttled);

        // 3. No battery info -> unthrottled
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, None, false, false, low_thresh);
        assert_eq!(fps, base_fps);
        assert!(!throttled);

        // 4. Normal battery above threshold (50%) -> unthrottled
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(50), false, false, low_thresh);
        assert_eq!(fps, base_fps);
        assert!(!throttled);

        // 5. Battery drops to 21% (above low_threshold) -> still unthrottled
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(21), false, false, low_thresh);
        assert_eq!(fps, base_fps);
        assert!(!throttled);

        // 6. Battery hits 20% (<= low_threshold) -> THROTTLES to 30 FPS
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(20), false, false, low_thresh);
        assert_eq!(fps, Some(30));
        assert!(throttled);

        // 7. Battery drops further to 15% -> remains throttled
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(15), false, true, low_thresh);
        assert_eq!(fps, Some(30));
        assert!(throttled);

        // 8. Battery charges to 21% (in deadband 21..24) while throttled -> STAYS THROTTLED (no flapping!)
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(21), false, true, low_thresh);
        assert_eq!(fps, Some(30));
        assert!(throttled);

        // 9. Battery charges to 24% (in deadband) while throttled -> STAYS THROTTLED
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(24), false, true, low_thresh);
        assert_eq!(fps, Some(30));
        assert!(throttled);

        // 10. Battery charges to 25% (>= low_threshold + 5) -> RELEASES throttle
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(25), false, true, low_thresh);
        assert_eq!(fps, base_fps);
        assert!(!throttled);

        // 11. Battery discharges to 24% while unthrottled -> stays unthrottled (deadband)
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(24), false, false, low_thresh);
        assert_eq!(fps, base_fps);
        assert!(!throttled);

        // 12. Plugged into AC while at 10% and throttled -> immediately releases
        let (fps, throttled) =
            compute_adaptive_target_fps(base_fps, true, Some(10), true, true, low_thresh);
        assert_eq!(fps, base_fps);
        assert!(!throttled);
    }
}
