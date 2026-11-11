//! Smart Auto-Pause foreground monitor and manual-pause precedence rules.

use crate::display::{DisplayRect, MonitorId, MonitorInfo};
use std::collections::BTreeMap;

#[cfg(windows)]
use std::time::Duration;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetSystemMetrics,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    IsZoomed, SM_CXSCREEN, SM_CYSCREEN, WS_EX_TOOLWINDOW,
};
#[cfg(windows)]
use windows::core::BOOL;

/// Observed top-level window geometry and monitor affinity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowObservation {
    pub primary_monitor_id: Option<MonitorId>,
    pub is_zoomed: bool,
    pub rect: DisplayRect,
}

/// Returns true when a window covers `mon_rect` (either maximized on that monitor or fullscreen bounding it).
pub fn is_window_covering_monitor(
    belongs_to_monitor: bool,
    is_zoomed: bool,
    win_rect: DisplayRect,
    mon_rect: DisplayRect,
) -> bool {
    if belongs_to_monitor && is_zoomed {
        return true;
    }
    if !win_rect.is_empty()
        && !mon_rect.is_empty()
        && win_rect.left <= mon_rect.left
        && win_rect.top <= mon_rect.top
        && win_rect.right >= mon_rect.right
        && win_rect.bottom >= mon_rect.bottom
    {
        return true;
    }
    false
}

/// Evaluates all `monitors` against visible top-level `observations` and returns `MonitorId -> is_covered`.
pub fn evaluate_monitor_coverage(
    monitors: &[MonitorInfo],
    observations: &[WindowObservation],
) -> BTreeMap<MonitorId, bool> {
    let mut coverage = BTreeMap::new();
    for m in monitors {
        coverage.insert(m.id.clone(), false);
    }
    for obs in observations {
        let mut any_uncovered = false;
        for m in monitors {
            if let Some(covered) = coverage.get_mut(&m.id)
                && !*covered
            {
                let belongs = obs.primary_monitor_id.as_ref() == Some(&m.id);
                if is_window_covering_monitor(belongs, obs.is_zoomed, obs.rect, m.rect) {
                    *covered = true;
                } else {
                    any_uncovered = true;
                }
            }
        }
        if !any_uncovered {
            break;
        }
    }
    coverage
}

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

/// Returns true if the window belongs to a Windows shell class that should be ignored for auto-pause.
///
/// Ignored shell classes:
/// - `"Progman"`
/// - `"WorkerW"`
/// - `"SHELLDLL_DefView"`
/// - `"Shell_TrayWnd"`
/// - `"Shell_SecondaryTrayWnd"`
/// - `"Windows.UI.Core.CoreWindow"`
///
/// # Safety
/// Caller must ensure `hwnd` is a valid Win32 window handle or null.
#[cfg(windows)]
pub unsafe fn is_ignored_shell_class(hwnd: HWND) -> bool {
    let mut class_buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    if len > 0 {
        let class_str = String::from_utf16_lossy(&class_buf[..len as usize]);
        matches!(
            class_str.as_str(),
            "Progman"
                | "WorkerW"
                | "SHELLDLL_DefView"
                | "Shell_TrayWnd"
                | "Shell_SecondaryTrayWnd"
                | "Windows.UI.Core.CoreWindow"
        )
    } else {
        false
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

        if is_ignored_shell_class(fg) {
            return false;
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

/// Collects visible top-level window observations via `EnumWindows` in Z-order.
///
/// # Safety
/// Caller must ensure Win32 subsystem is initialized and current thread can invoke Win32 APIs.
#[cfg(windows)]
pub unsafe fn collect_window_observations(
    monitors: &[MonitorInfo],
    our_pid: u32,
) -> Vec<WindowObservation> {
    struct EnumData<'a> {
        monitors: &'a [MonitorInfo],
        our_pid: u32,
        observations: Vec<WindowObservation>,
    }

    unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = unsafe { &mut *(lparam.0 as *mut EnumData) };

        if hwnd.0.is_null() {
            return BOOL(1);
        }

        if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
            return BOOL(1);
        }

        let mut win_pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut win_pid)) };
        if win_pid == data.our_pid {
            return BOOL(1);
        }

        let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        if (ex_style & WS_EX_TOOLWINDOW.0) != 0 {
            return BOOL(1);
        }

        if unsafe { is_ignored_shell_class(hwnd) } {
            return BOOL(1);
        }

        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
            return BOOL(1);
        }

        let win_rect = DisplayRect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };

        if win_rect.is_empty() {
            return BOOL(1);
        }

        let is_zoomed = unsafe { IsZoomed(hwnd) }.as_bool();

        let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut primary_monitor_id = None;
        if !hmon.0.is_null() {
            let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
                let rc = DisplayRect {
                    left: mi.rcMonitor.left,
                    top: mi.rcMonitor.top,
                    right: mi.rcMonitor.right,
                    bottom: mi.rcMonitor.bottom,
                };
                primary_monitor_id = data
                    .monitors
                    .iter()
                    .find(|m| m.rect == rc)
                    .map(|m| m.id.clone())
                    .or_else(|| {
                        data.monitors
                            .iter()
                            .map(|m| (m, m.rect.intersection_area(rc)))
                            .filter(|(_, area)| *area > 0)
                            .max_by_key(|(_, area)| *area)
                            .map(|(m, _)| m.id.clone())
                    });
            }
        }

        if primary_monitor_id.is_none() {
            primary_monitor_id = data
                .monitors
                .iter()
                .map(|m| (m, m.rect.intersection_area(win_rect)))
                .filter(|(_, area)| *area > 0)
                .max_by_key(|(_, area)| *area)
                .map(|(m, _)| m.id.clone());
        }

        data.observations.push(WindowObservation {
            primary_monitor_id,
            is_zoomed,
            rect: win_rect,
        });

        BOOL(1)
    }

    let mut data = EnumData {
        monitors,
        our_pid,
        observations: Vec::with_capacity(32),
    };

    let _ = unsafe {
        EnumWindows(
            Some(enum_windows_proc),
            LPARAM(&mut data as *mut _ as isize),
        )
    };

    data.observations
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

/// Spawns a background multi-monitor auto-pause monitor thread (1000ms polling interval).
///
/// Calls `on_cover_change` whenever the per-monitor coverage state changes.
#[cfg(windows)]
pub fn spawn_multi_monitor_thread<F>(
    monitors_snapshot: std::sync::Arc<std::sync::RwLock<Vec<MonitorInfo>>>,
    on_cover_change: F,
) where
    F: Fn(BTreeMap<MonitorId, bool>) + Send + 'static,
{
    let our_pid = std::process::id();
    std::thread::spawn(move || {
        let mut last_coverage: Option<BTreeMap<MonitorId, bool>> = None;
        loop {
            std::thread::sleep(Duration::from_millis(1000));

            let monitors = match monitors_snapshot.read() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };

            let observations = unsafe { collect_window_observations(&monitors, our_pid) };
            let coverage = evaluate_monitor_coverage(&monitors, &observations);

            if last_coverage.as_ref() != Some(&coverage) {
                last_coverage = Some(coverage.clone());
                on_cover_change(coverage);
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

    #[test]
    fn test_per_monitor_coverage_matrix_two_and_three_monitors() {
        let mon_a = MonitorInfo {
            id: MonitorId("MON_A".into()),
            friendly_name: "Display 1".into(),
            gdi_name: r"\\.\DISPLAY1".into(),
            rect: DisplayRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: true,
        };
        let mon_b = MonitorInfo {
            id: MonitorId("MON_B".into()),
            friendly_name: "Display 2".into(),
            gdi_name: r"\\.\DISPLAY2".into(),
            rect: DisplayRect {
                left: 1920,
                top: 0,
                right: 3840,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };
        let mon_c = MonitorInfo {
            id: MonitorId("MON_C".into()),
            friendly_name: "Display 3".into(),
            gdi_name: r"\\.\DISPLAY3".into(),
            rect: DisplayRect {
                left: 3840,
                top: 0,
                right: 5760,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };
        let monitors = vec![mon_a.clone(), mon_b.clone(), mon_c.clone()];

        // 1. Maximized window on Monitor A leaves Monitor B and Monitor C false
        //    (even when a non-maximized window is also open on Monitor B!).
        let obs_a_maximized = WindowObservation {
            primary_monitor_id: Some(mon_a.id.clone()),
            is_zoomed: true,
            rect: mon_a.rect,
        };
        let obs_b_normal = WindowObservation {
            primary_monitor_id: Some(mon_b.id.clone()),
            is_zoomed: false,
            rect: DisplayRect {
                left: 2000,
                top: 100,
                right: 3000,
                bottom: 800,
            },
        };
        let coverage1 = evaluate_monitor_coverage(&monitors, &[obs_a_maximized, obs_b_normal]);
        assert_eq!(coverage1.get(&mon_a.id), Some(&true));
        assert_eq!(coverage1.get(&mon_b.id), Some(&false));
        assert_eq!(coverage1.get(&mon_c.id), Some(&false));

        // 2. Switching maximized window to Monitor B pauses only B.
        let obs_a_normal = WindowObservation {
            primary_monitor_id: Some(mon_a.id.clone()),
            is_zoomed: false,
            rect: DisplayRect {
                left: 100,
                top: 100,
                right: 800,
                bottom: 600,
            },
        };
        let obs_b_maximized = WindowObservation {
            primary_monitor_id: Some(mon_b.id.clone()),
            is_zoomed: true,
            rect: mon_b.rect,
        };
        let coverage2 = evaluate_monitor_coverage(&monitors, &[obs_a_normal, obs_b_maximized]);
        assert_eq!(coverage2.get(&mon_a.id), Some(&false));
        assert_eq!(coverage2.get(&mon_b.id), Some(&true));
        assert_eq!(coverage2.get(&mon_c.id), Some(&false));

        // 3. Borderless fullscreen spanning A+B pauses both A and B while leaving C running.
        let obs_spanning_ab = WindowObservation {
            primary_monitor_id: Some(mon_a.id.clone()),
            is_zoomed: false,
            rect: DisplayRect {
                left: 0,
                top: 0,
                right: 3840,
                bottom: 1080,
            },
        };
        let coverage3 = evaluate_monitor_coverage(&monitors, &[obs_spanning_ab]);
        assert_eq!(coverage3.get(&mon_a.id), Some(&true));
        assert_eq!(coverage3.get(&mon_b.id), Some(&true));
        assert_eq!(coverage3.get(&mon_c.id), Some(&false));
    }

    #[test]
    fn test_negative_coordinate_fullscreen_window_covers_only_target_monitor() {
        let mon_b = MonitorInfo {
            id: MonitorId("MON_B_LEFT".into()),
            friendly_name: "Left Monitor".into(),
            gdi_name: r"\\.\DISPLAY2".into(),
            rect: DisplayRect {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };
        let mon_a = MonitorInfo {
            id: MonitorId("MON_A_PRIMARY".into()),
            friendly_name: "Primary Monitor".into(),
            gdi_name: r"\\.\DISPLAY1".into(),
            rect: DisplayRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: true,
        };
        let monitors = vec![mon_b.clone(), mon_a.clone()];

        // Monitor B at (-1920, 0, 0, 1080) covered by fullscreen (-1920, 0, 0, 1080)
        // sets B = true and primary Monitor A (0, 0, 1920, 1080) = false.
        let obs_b_fullscreen = WindowObservation {
            primary_monitor_id: Some(mon_b.id.clone()),
            is_zoomed: false,
            rect: DisplayRect {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            },
        };
        let coverage1 = evaluate_monitor_coverage(&monitors, &[obs_b_fullscreen]);
        assert_eq!(coverage1.get(&mon_b.id), Some(&true));
        assert_eq!(coverage1.get(&mon_a.id), Some(&false));

        // Small window on negative Monitor B does not cover B.
        let obs_b_small = WindowObservation {
            primary_monitor_id: Some(mon_b.id.clone()),
            is_zoomed: false,
            rect: DisplayRect {
                left: -1500,
                top: 100,
                right: -500,
                bottom: 800,
            },
        };
        let coverage2 = evaluate_monitor_coverage(&monitors, &[obs_b_small]);
        assert_eq!(coverage2.get(&mon_b.id), Some(&false));
        assert_eq!(coverage2.get(&mon_a.id), Some(&false));
    }

    #[test]
    fn test_is_window_covering_monitor_edge_cases() {
        let mon = DisplayRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };

        // Maximized and belonging to monitor: true
        assert!(is_window_covering_monitor(true, true, mon, mon));
        // Maximized but belonging to different monitor: false if not bounding
        let other_rect = DisplayRect {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1080,
        };
        assert!(!is_window_covering_monitor(false, true, other_rect, mon));

        // Exact match fullscreen bounds: true
        assert!(is_window_covering_monitor(false, false, mon, mon));

        // Superset fullscreen bounds: true
        let superset = DisplayRect {
            left: -10,
            top: -10,
            right: 2000,
            bottom: 1100,
        };
        assert!(is_window_covering_monitor(false, false, superset, mon));

        // Partial overlap (e.g. half-screen snap): false
        let half = DisplayRect {
            left: 0,
            top: 0,
            right: 960,
            bottom: 1080,
        };
        assert!(!is_window_covering_monitor(true, false, half, mon));

        // Empty window or empty monitor: false
        let empty_win = DisplayRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        assert!(!is_window_covering_monitor(false, false, empty_win, mon));
        let empty_mon = DisplayRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 0,
        };
        assert!(!is_window_covering_monitor(false, false, mon, empty_mon));
    }

    #[cfg(windows)]
    #[test]
    fn test_collect_window_observations_smoke() {
        let monitors = vec![MonitorInfo {
            id: MonitorId("SMOKE_MON".into()),
            friendly_name: "Smoke Monitor".into(),
            gdi_name: r"\\.\DISPLAY1".into(),
            rect: DisplayRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: true,
        }];
        let observations = unsafe { collect_window_observations(&monitors, std::process::id()) };
        // Smoke test: verifies EnumWindows runs, parses windows, maps monitors without panicking.
        let coverage = evaluate_monitor_coverage(&monitors, &observations);
        assert!(coverage.contains_key(&monitors[0].id));
    }
}
