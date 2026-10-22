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

    /// Trims the process working set, commanding the Windows kernel to page out unused private bytes.
    pub fn trim_working_set() {
        unsafe {
            let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        }
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
}
