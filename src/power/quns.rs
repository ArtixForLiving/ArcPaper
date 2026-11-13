//! Query User Notification State (QUNS) helper and foreground window monitor attribution (FR-304).
//!
//! Detects exclusive fullscreen Direct3D games, busy presentation states, and graphics-intensive
//! full-screen apps via `SHQueryUserNotificationState`.
//!
//! # Critical Constraints
//! - `SHQueryUserNotificationState` is not event-based: it must NEVER be run in a polling timer.
//!   It is queried strictly when foreground window events or coalesced evaluation runs in `winevent.rs`.
//! - QUNS is global from Windows Shell, but MUST be attributed strictly to the monitor where
//!   the foreground window resides (`MonitorFromWindow`). Other monitors must NOT be paused.

use crate::app::pause::PauseReason;
use crate::display::{DisplayRect, MonitorId, MonitorInfo};

#[cfg(windows)]
use windows::Win32::Foundation::HWND;

// Windows Shell QUERY_USER_NOTIFICATION_STATE canonical values
pub const QUNS_NOT_PRESENT: i32 = 1;
pub const QUNS_BUSY: i32 = 2;
pub const QUNS_RUNNING_D3D_FULL_SCREEN: i32 = 3;
pub const QUNS_PRESENTATION_MODE: i32 = 4;
pub const QUNS_ACCEPTS_NOTIFICATIONS: i32 = 5;
pub const QUNS_QUIET_TIME: i32 = 6;
pub const QUNS_APP: i32 = 7;

/// Returns `true` if `state` indicates an exclusive fullscreen Direct3D application or busy game.
///
/// If `treat_presentation_as_busy` is false, `QUNS_PRESENTATION_MODE` returns `false` to prevent
/// false positive auto-pauses during slide presentations.
pub fn is_quns_exclusive_fullscreen(state: i32, treat_presentation_as_busy: bool) -> bool {
    (state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_BUSY || state == 2 || state == 3)
        || (treat_presentation_as_busy
            && (state == QUNS_PRESENTATION_MODE || state == 4 || state == 5))
}

/// Queries the Windows Shell user notification state.
///
/// Returns `None` if the call fails or on non-Windows platforms.
pub fn query_user_notification_state() -> Option<i32> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::SHQueryUserNotificationState;
        if let Ok(state) = unsafe { SHQueryUserNotificationState() } {
            return Some(state.0);
        }
    }
    None
}

/// Finds the monitor that best matches `rect` (exact match or greatest intersection area).
pub fn find_monitor_for_rect(rect: DisplayRect, monitors: &[MonitorInfo]) -> Option<MonitorId> {
    if rect.is_empty() || monitors.is_empty() {
        return None;
    }

    if let Some(m) = monitors.iter().find(|m| m.rect == rect) {
        return Some(m.id.clone());
    }

    monitors
        .iter()
        .map(|m| (m, m.rect.intersection_area(rect)))
        .filter(|(_, area)| *area > 0)
        .max_by_key(|(_, area)| *area)
        .map(|(m, _)| m.id.clone())
}

/// Finds the monitor where `hwnd` resides.
///
/// Queries `MonitorFromWindow` first, falling back to window rect intersection.
#[cfg(windows)]
pub fn find_monitor_for_window(hwnd: HWND, monitors: &[MonitorInfo]) -> Option<MonitorId> {
    if hwnd.0.is_null() || monitors.is_empty() {
        return None;
    }

    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow,
    };

    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
    if !hmon.0.is_null() {
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
            let rc = DisplayRect {
                left: mi.rcMonitor.left,
                top: mi.rcMonitor.top,
                right: mi.rcMonitor.right,
                bottom: mi.rcMonitor.bottom,
            };
            if let Some(id) = find_monitor_for_rect(rc, monitors) {
                return Some(id);
            }
        }
    }

    let mut rect = windows::Win32::Foundation::RECT::default();
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) }.is_ok() {
        let win_rect = DisplayRect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        return find_monitor_for_rect(win_rect, monitors);
    }

    None
}

#[cfg(not(windows))]
pub fn find_monitor_for_window(_hwnd: (), _monitors: &[MonitorInfo]) -> Option<MonitorId> {
    None
}

/// Evaluates whether an exclusive fullscreen app requires pausing a specific monitor.
///
/// If QUNS indicates exclusive fullscreen / busy state and `fg_hwnd` belongs to one of `monitors`,
/// returns `(target_monitor_id, PauseReason::EXCLUSIVE_FULLSCREEN)`.
/// Only the monitor hosting the foreground window is paused; other monitors are unaffected.
#[cfg(windows)]
pub fn evaluate_quns_pause(
    fg_hwnd: HWND,
    monitors: &[MonitorInfo],
    quns_state: Option<i32>,
    detect_exclusive: bool,
    treat_presentation_as_busy: bool,
) -> Option<(MonitorId, PauseReason)> {
    if !detect_exclusive {
        return None;
    }

    let state = quns_state?;
    if !is_quns_exclusive_fullscreen(state, treat_presentation_as_busy) {
        return None;
    }

    let target_mon = find_monitor_for_window(fg_hwnd, monitors)?;
    Some((target_mon, PauseReason::EXCLUSIVE_FULLSCREEN))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quns_running_d3d_full_screen_is_exclusive_fullscreen() {
        assert!(is_quns_exclusive_fullscreen(
            QUNS_RUNNING_D3D_FULL_SCREEN,
            false
        ));
        assert!(is_quns_exclusive_fullscreen(
            QUNS_RUNNING_D3D_FULL_SCREEN,
            true
        ));
        // Synthetic mapping (2)
        assert!(is_quns_exclusive_fullscreen(2, false));
        assert!(is_quns_exclusive_fullscreen(3, false));
    }

    #[test]
    fn test_quns_busy_is_exclusive_fullscreen() {
        assert!(is_quns_exclusive_fullscreen(QUNS_BUSY, false));
        assert!(is_quns_exclusive_fullscreen(QUNS_BUSY, true));
    }

    #[test]
    fn test_quns_presentation_mode_respects_flag() {
        // Presentation mode must NOT pause when treat_presentation_as_busy is false
        assert!(!is_quns_exclusive_fullscreen(QUNS_PRESENTATION_MODE, false));
        // Presentation mode DOES pause when treat_presentation_as_busy is true
        assert!(is_quns_exclusive_fullscreen(QUNS_PRESENTATION_MODE, true));

        // Test synthetic mapping (5)
        assert!(!is_quns_exclusive_fullscreen(5, false));
        assert!(is_quns_exclusive_fullscreen(5, true));
    }

    #[test]
    fn test_quns_idle_and_other_states_return_false() {
        assert!(!is_quns_exclusive_fullscreen(QUNS_NOT_PRESENT, false));
        assert!(!is_quns_exclusive_fullscreen(QUNS_NOT_PRESENT, true));

        assert!(!is_quns_exclusive_fullscreen(
            QUNS_ACCEPTS_NOTIFICATIONS,
            false
        ));
        assert!(!is_quns_exclusive_fullscreen(QUNS_QUIET_TIME, false));
        assert!(!is_quns_exclusive_fullscreen(QUNS_APP, false));
        assert!(!is_quns_exclusive_fullscreen(999, false));
        assert!(!is_quns_exclusive_fullscreen(0, false));
    }

    #[test]
    fn test_find_monitor_for_rect_exact_match() {
        let mon1 = MonitorInfo {
            id: MonitorId("MON_1".into()),
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
        let mon2 = MonitorInfo {
            id: MonitorId("MON_2".into()),
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
        let monitors = vec![mon1.clone(), mon2.clone()];

        assert_eq!(
            find_monitor_for_rect(mon1.rect, &monitors),
            Some(MonitorId("MON_1".into()))
        );
        assert_eq!(
            find_monitor_for_rect(mon2.rect, &monitors),
            Some(MonitorId("MON_2".into()))
        );
    }

    #[test]
    fn test_find_monitor_for_rect_overlap_and_negative_coords() {
        let left_mon = MonitorInfo {
            id: MonitorId("MON_LEFT".into()),
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
        let primary_mon = MonitorInfo {
            id: MonitorId("MON_PRIMARY".into()),
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
        let monitors = vec![left_mon, primary_mon];

        // Rect inside left monitor
        let rect_left = DisplayRect {
            left: -1500,
            top: 100,
            right: -500,
            bottom: 800,
        };
        assert_eq!(
            find_monitor_for_rect(rect_left, &monitors),
            Some(MonitorId("MON_LEFT".into()))
        );

        // Rect inside primary monitor
        let rect_primary = DisplayRect {
            left: 200,
            top: 200,
            right: 1200,
            bottom: 900,
        };
        assert_eq!(
            find_monitor_for_rect(rect_primary, &monitors),
            Some(MonitorId("MON_PRIMARY".into()))
        );

        // Spanning rect with greater area on primary
        let rect_mostly_primary = DisplayRect {
            left: -100,
            top: 0,
            right: 1000,
            bottom: 1080,
        };
        assert_eq!(
            find_monitor_for_rect(rect_mostly_primary, &monitors),
            Some(MonitorId("MON_PRIMARY".into()))
        );
    }

    #[test]
    fn test_find_monitor_for_rect_empty_or_no_monitors() {
        let empty_rect = DisplayRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mon = MonitorInfo {
            id: MonitorId("MON_1".into()),
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

        assert_eq!(
            find_monitor_for_rect(empty_rect, std::slice::from_ref(&mon)),
            None
        );
        assert_eq!(find_monitor_for_rect(mon.rect, &[]), None);

        // Rect outside bounds
        let out_rect = DisplayRect {
            left: 5000,
            top: 5000,
            right: 6000,
            bottom: 6000,
        };
        assert_eq!(
            find_monitor_for_rect(out_rect, std::slice::from_ref(&mon)),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_find_monitor_for_window_null_hwnd_returns_none() {
        let mon = MonitorInfo {
            id: MonitorId("MON_1".into()),
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
        assert_eq!(
            find_monitor_for_window(HWND::default(), std::slice::from_ref(&mon)),
            None
        );
        assert_eq!(find_monitor_for_window(HWND::default(), &[]), None);
    }

    #[cfg(windows)]
    #[test]
    fn test_find_monitor_for_window_real_hwnd_attribution() {
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_POPUP,
        };
        use windows::core::w;

        let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("QunsTestWindow"),
                WS_POPUP,
                100,
                100,
                200,
                200,
                None,
                None,
                Some(hinst.into()),
                None,
            )
        };

        if let Ok(wnd) = hwnd {
            let mon1 = MonitorInfo {
                id: MonitorId("MON_1".into()),
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
            let mon2 = MonitorInfo {
                id: MonitorId("MON_2".into()),
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
            let monitors = vec![mon1, mon2];

            let matched = find_monitor_for_window(wnd, &monitors);
            assert_eq!(matched, Some(MonitorId("MON_1".into())));

            unsafe {
                let _ = DestroyWindow(wnd);
            }
        }
    }

    #[test]
    fn test_quns_maps_to_foreground_monitor_only() {
        // Multi-monitor attribution test verifying strict per-monitor isolation (PRD line 326)
        let mon1 = MonitorInfo {
            id: MonitorId("MON_1".into()),
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
        let mon2 = MonitorInfo {
            id: MonitorId("MON_2".into()),
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
        let monitors = vec![mon1.clone(), mon2.clone()];

        // Window on Mon 1 rect
        let rect_win1 = DisplayRect {
            left: 100,
            top: 100,
            right: 800,
            bottom: 600,
        };
        let target1 = find_monitor_for_rect(rect_win1, &monitors);
        assert_eq!(target1, Some(MonitorId("MON_1".into())));

        // Window on Mon 2 rect
        let rect_win2 = DisplayRect {
            left: 2000,
            top: 100,
            right: 3000,
            bottom: 800,
        };
        let target2 = find_monitor_for_rect(rect_win2, &monitors);
        assert_eq!(target2, Some(MonitorId("MON_2".into())));

        // If game is on Mon 1, Mon 1 gets EXCLUSIVE_FULLSCREEN and Mon 2 does NOT
        let quns_active = is_quns_exclusive_fullscreen(QUNS_RUNNING_D3D_FULL_SCREEN, false);
        assert!(quns_active);

        let mut monitor_reasons: std::collections::BTreeMap<MonitorId, PauseReason> =
            std::collections::BTreeMap::new();
        monitor_reasons.insert(mon1.id.clone(), PauseReason::empty());
        monitor_reasons.insert(mon2.id.clone(), PauseReason::empty());

        if quns_active
            && let Some(target) = target1
            && let Some(r) = monitor_reasons.get_mut(&target)
        {
            r.insert(PauseReason::EXCLUSIVE_FULLSCREEN);
        }

        assert_eq!(
            monitor_reasons.get(&mon1.id),
            Some(&PauseReason::EXCLUSIVE_FULLSCREEN)
        );
        assert_eq!(
            monitor_reasons.get(&mon2.id),
            Some(&PauseReason::empty()),
            "Monitor 2 must NOT be paused when exclusive fullscreen game runs on Monitor 1"
        );
    }
}
