//! Smart Auto-Pause foreground monitor and manual-pause precedence rules.

#[cfg(windows)]
use std::time::Duration;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, RECT};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, IsZoomed, SM_CXSCREEN, SM_CYSCREEN,
};

/// Computes whether playback must be paused given foreground coverage and manual pause state.
pub fn resolve_effective_pause(is_covered: bool, is_manually_paused: bool) -> bool {
    is_covered || is_manually_paused
}

/// Resolves playback pause state and `is_auto_paused` flag when the user toggles manual pause from the tray.
pub fn apply_manual_pause_toggle(is_covered: bool, next_manually_paused: bool) -> (bool, bool) {
    let effective_paused = resolve_effective_pause(is_covered, next_manually_paused);
    let next_auto_paused = !next_manually_paused && is_covered;
    (effective_paused, next_auto_paused)
}

/// Computes the next `(effective_paused, next_is_auto_paused, optional_log_msg)` tuple.
pub fn compute_autopause_transition(
    is_covered: bool,
    is_manually_paused: bool,
    was_auto_paused: bool,
) -> (bool, bool, Option<&'static str>) {
    let effective_paused = resolve_effective_pause(is_covered, is_manually_paused);
    if is_manually_paused {
        return (true, false, None);
    }

    if is_covered && !was_auto_paused {
        (
            effective_paused,
            true,
            Some(
                "[ArcPaper] Smart Auto-Pause: Maximized / fullscreen window active. Wallpaper paused.",
            ),
        )
    } else if !is_covered && was_auto_paused {
        (
            effective_paused,
            false,
            Some("[ArcPaper] Smart Auto-Resume: Desktop visible. Wallpaper resumed."),
        )
    } else {
        (effective_paused, was_auto_paused, None)
    }
}

/// Checks whether the active foreground window covers the desktop screen (maximized or fullscreen).
///
/// # Safety
/// Caller must pass a valid Win32 `HWND` obtained from `GetForegroundWindow`.
#[cfg(windows)]
pub unsafe fn is_foreground_covering_screen(
    fg: HWND,
    screen_w: i32,
    screen_h: i32,
    our_pid: u32,
) -> bool {
    unsafe {
        if fg.0.is_null() {
            return false;
        }
        if !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
            return false;
        }

        let mut fg_pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        if fg_pid == our_pid {
            return false;
        }

        let mut class_buf = [0u16; 256];
        let len = GetClassNameW(fg, &mut class_buf);
        if len > 0 {
            let class_str = String::from_utf16_lossy(&class_buf[..len as usize]);
            match class_str.as_str() {
                "Progman"
                | "WorkerW"
                | "SHELLDLL_DefView"
                | "Shell_TrayWnd"
                | "Shell_SecondaryTrayWnd"
                | "Windows.UI.Core.CoreWindow" => {
                    return false;
                }
                _ => {}
            }
        }

        if IsZoomed(fg).as_bool() {
            return true;
        }

        let mut rect = RECT::default();
        if GetWindowRect(fg, &mut rect).is_ok()
            && rect.left <= 0
            && rect.top <= 0
            && rect.right >= screen_w
            && rect.bottom >= screen_h
        {
            return true;
        }

        false
    }
}

/// Spawns the lightweight background foreground window monitor thread (1000ms interval).
#[cfg(windows)]
pub fn spawn_monitor_thread<F>(on_cover_change: F)
where
    F: Fn(bool) + Send + 'static,
{
    let our_pid = std::process::id();
    std::thread::spawn(move || {
        let mut last_state = false;
        let mut last_fg = HWND::default();
        let mut last_zoomed = false;
        loop {
            std::thread::sleep(Duration::from_millis(1000));

            unsafe {
                let fg = GetForegroundWindow();
                if fg.0.is_null() {
                    if last_state {
                        last_state = false;
                        last_fg = HWND::default();
                        last_zoomed = false;
                        on_cover_change(false);
                    }
                    continue;
                }

                let is_zoomed = IsZoomed(fg).as_bool();
                if fg == last_fg && is_zoomed == last_zoomed {
                    continue;
                }

                last_fg = fg;
                last_zoomed = is_zoomed;

                let screen_w = GetSystemMetrics(SM_CXSCREEN);
                let screen_h = GetSystemMetrics(SM_CYSCREEN);
                let is_covered = is_foreground_covering_screen(fg, screen_w, screen_h, our_pid);
                if is_covered != last_state {
                    last_state = is_covered;
                    on_cover_change(is_covered);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_effective_pause_matrix_covered_x_manual() {
        assert!(!resolve_effective_pause(false, false));
        assert!(resolve_effective_pause(true, false));
        assert!(resolve_effective_pause(false, true));
        assert!(resolve_effective_pause(true, true));
    }

    #[test]
    fn test_autopause_transition_preserves_manual_pause() {
        let (eff1, auto1, log1) = compute_autopause_transition(true, true, false);
        assert!(eff1);
        assert!(!auto1);
        assert!(log1.is_none());

        let (eff2, auto2, log2) = compute_autopause_transition(false, true, auto1);
        assert!(
            eff2,
            "manual pause must remain paused when window un-maximizes"
        );
        assert!(!auto2);
        assert!(log2.is_none());

        let (eff3, auto3, log3) = compute_autopause_transition(true, false, false);
        assert!(eff3);
        assert!(auto3);
        assert!(log3.is_some());

        let (eff4, auto4, log4) = compute_autopause_transition(false, false, auto3);
        assert!(!eff4);
        assert!(!auto4);
        assert!(log4.is_some());
    }

    #[test]
    fn test_manual_resume_while_covered_stays_paused() {
        assert_eq!(
            apply_manual_pause_toggle(true, false),
            (true, true),
            "manual resume when covered must keep playback paused with is_auto_paused=true"
        );
        assert_eq!(
            apply_manual_pause_toggle(true, true),
            (true, false),
            "manual pause when covered keeps paused with is_auto_paused=false"
        );
    }

    #[test]
    fn test_manual_resume_while_uncovered_resumes() {
        assert_eq!(
            apply_manual_pause_toggle(false, false),
            (false, false),
            "manual resume when uncovered must resume playback with is_auto_paused=false"
        );
        assert_eq!(
            apply_manual_pause_toggle(false, true),
            (true, false),
            "manual pause when uncovered pauses playback with is_auto_paused=false"
        );
    }
}
