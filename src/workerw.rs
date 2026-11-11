use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, SMTO_NORMAL, SendMessageTimeoutW,
};
use windows::core::{PCWSTR, w};

use crate::display::DisplayRect;

/// Attempts to attach the current thread to the interactive "Default" desktop
/// if the process is running in a secondary window station or sandbox environment.
pub fn attach_to_default_desktop() -> bool {
    type FnOpenDesktopW = unsafe extern "system" fn(*const u16, u32, i32, u32) -> isize;
    type FnSetThreadDesktop = unsafe extern "system" fn(isize) -> i32;
    type FnCloseDesktop = unsafe extern "system" fn(isize) -> i32;

    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        if let Ok(user32) = LoadLibraryW(w!("user32.dll")) {
            let p_open = GetProcAddress(user32, windows::core::s!("OpenDesktopW"));
            let p_set = GetProcAddress(user32, windows::core::s!("SetThreadDesktop"));
            let p_close = GetProcAddress(user32, windows::core::s!("CloseDesktop"));
            if let (Some(open_fn), Some(set_fn), Some(close_fn)) = (p_open, p_set, p_close) {
                let open_desktop: FnOpenDesktopW = std::mem::transmute(open_fn);
                let set_thread_desktop: FnSetThreadDesktop = std::mem::transmute(set_fn);
                let close_desktop: FnCloseDesktop = std::mem::transmute(close_fn);
                let name: Vec<u16> = "Default\0".encode_utf16().collect();
                // 0x01FF covers standard DESKTOP rights
                let hdesk = open_desktop(name.as_ptr(), 0, 0, 0x01FF);
                if hdesk != 0 {
                    let success = set_thread_desktop(hdesk) != 0;
                    let _ = close_desktop(hdesk);
                    return success;
                }
            }
        }
    }
    false
}

/// Finds the `Progman` window handle in the current thread desktop.
pub fn get_progman_hwnd() -> HWND {
    unsafe {
        match FindWindowW(w!("Progman"), PCWSTR::null()) {
            Ok(h) if !h.0.is_null() => h,
            _ => HWND::default(),
        }
    }
}

/// Internal logic to find or spawn the desktop WorkerW handle from a given Progman handle.
fn get_workerw_from_progman(progman: HWND) -> HWND {
    unsafe {
        if progman.0.is_null() {
            return HWND::default();
        }

        // Send 0x052C message to Progman (wParam 0xD, lParam 0x1) to spawn WorkerW
        let mut result = 0usize;
        let _ = SendMessageTimeoutW(
            progman,
            0x052C,
            WPARAM(0xD),
            LPARAM(0x1),
            SMTO_NORMAL,
            1000,
            Some(&mut result),
        );

        // Fallback message with 0, 0 for compatibility
        let _ = SendMessageTimeoutW(
            progman,
            0x052C,
            WPARAM(0),
            LPARAM(0),
            SMTO_NORMAL,
            1000,
            Some(&mut result),
        );

        // Case A: Windows 11 (24H2 / 25H2) where WorkerW is a child inside Progman:
        if let Ok(worker_in_progman) =
            FindWindowExW(Some(progman), None, w!("WorkerW"), PCWSTR::null())
            && !worker_in_progman.0.is_null()
        {
            return worker_in_progman;
        }

        // Case B: Windows 10 / earlier Windows 11 where WorkerW is a sibling of the WorkerW hosting SHELLDLL_DefView:
        let mut top_worker =
            FindWindowExW(None, None, w!("WorkerW"), PCWSTR::null()).unwrap_or(HWND::default());
        while !top_worker.0.is_null() {
            let child_shell = FindWindowExW(
                Some(top_worker),
                None,
                w!("SHELLDLL_DefView"),
                PCWSTR::null(),
            )
            .unwrap_or(HWND::default());
            if !child_shell.0.is_null() {
                let next_worker =
                    FindWindowExW(None, Some(top_worker), w!("WorkerW"), PCWSTR::null())
                        .unwrap_or(HWND::default());
                if !next_worker.0.is_null() {
                    return next_worker;
                }
            }
            top_worker = FindWindowExW(None, Some(top_worker), w!("WorkerW"), PCWSTR::null())
                .unwrap_or(HWND::default());
        }

        // Case C: Any top-level WorkerW
        let any_worker =
            FindWindowExW(None, None, w!("WorkerW"), PCWSTR::null()).unwrap_or(HWND::default());
        if !any_worker.0.is_null() {
            return any_worker;
        }

        // Fallback: Return Progman if WorkerW is not available
        progman
    }
}

fn get_desktop_workerw_hwnd_internal() -> HWND {
    let progman = get_progman_hwnd();
    if !progman.0.is_null() {
        return get_workerw_from_progman(progman);
    }

    // If running in a background sandbox session, probe Default desktop on a separate thread
    // so the calling thread's desktop and WebView2 are never disturbed.
    let raw = std::thread::spawn(|| {
        attach_to_default_desktop();
        let p = get_progman_hwnd();
        if !p.0.is_null() {
            get_workerw_from_progman(p).0 as usize
        } else {
            0usize
        }
    })
    .join()
    .unwrap_or(0usize);

    HWND(raw as *mut _)
}

/// Safely attaches to the desktop WorkerW window handle.
/// Retries up to 10 times with a 500ms delay if Progman or WorkerW is not yet ready during system startup.
pub fn attach_to_desktop() -> HWND {
    for attempt in 1..=10 {
        let progman = get_progman_hwnd();
        if !progman.0.is_null() {
            let workerw = get_workerw_from_progman(progman);
            if !workerw.0.is_null() {
                return workerw;
            }
        }

        let fallback_workerw = get_desktop_workerw_hwnd_internal();
        if !fallback_workerw.0.is_null() {
            return fallback_workerw;
        }

        if attempt < 10 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    HWND::default()
}

/// PascalCase alias for attach_to_desktop matching Windows desktop integration conventions.
#[allow(non_snake_case)]
pub fn AttachToDesktop() -> HWND {
    attach_to_desktop()
}

/// Attaches to the desktop `WorkerW`, configures pass-through styles and physical dimensions,
/// and returns the target window handle (`WorkerW`, `Progman` fallback, or null in sandbox).
pub fn attach_and_configure_desktop_surface(screen_w: i32, screen_h: i32) -> HWND {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER,
        SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_CLIPCHILDREN,
        WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
    };

    let workerw_hwnd = attach_to_desktop();
    if !workerw_hwnd.0.is_null() {
        unsafe {
            let p_style = GetWindowLongPtrW(workerw_hwnd, GWL_STYLE) as u32;
            let _ = SetWindowLongPtrW(
                workerw_hwnd,
                GWL_STYLE,
                (p_style | WS_CLIPCHILDREN.0) as isize,
            );
            let p_ex_style = GetWindowLongPtrW(workerw_hwnd, GWL_EXSTYLE) as u32;
            let _ = SetWindowLongPtrW(
                workerw_hwnd,
                GWL_EXSTYLE,
                (p_ex_style | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as isize,
            );
            let _ = SetWindowPos(
                workerw_hwnd,
                None,
                0,
                0,
                screen_w,
                screen_h,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = ShowWindow(workerw_hwnd, SW_SHOW);
        }
    }

    let progman_current = get_progman_hwnd();
    if !workerw_hwnd.0.is_null() {
        workerw_hwnd
    } else if !progman_current.0.is_null() {
        progman_current
    } else {
        HWND::default()
    }
}

/// Pure helper converting virtual screen monitor coordinates into client coordinates relative to an origin.
pub fn virtual_rect_to_client_coords(
    rect: DisplayRect,
    origin_x: i32,
    origin_y: i32,
) -> (i32, i32, i32, i32) {
    (
        rect.left - origin_x,
        rect.top - origin_y,
        rect.width(),
        rect.height(),
    )
}

/// Attaches to the desktop `WorkerW` (or `Progman` fallback), configures pass-through styles without
/// shrinking a multi-monitor `WorkerW`, and returns the desktop parent HWND.
pub fn attach_virtual_desktop_surface() -> HWND {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GWL_STYLE, GetParent, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
        SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW,
        SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_CLIPCHILDREN, WS_EX_NOACTIVATE,
        WS_EX_TRANSPARENT,
    };

    let workerw_hwnd = attach_to_desktop();
    let progman_current = get_progman_hwnd();

    let target_hwnd = if !workerw_hwnd.0.is_null() {
        workerw_hwnd
    } else if !progman_current.0.is_null() {
        progman_current
    } else {
        return HWND::default();
    };

    unsafe {
        let p_style = GetWindowLongPtrW(target_hwnd, GWL_STYLE) as u32;
        let _ = SetWindowLongPtrW(
            target_hwnd,
            GWL_STYLE,
            (p_style | WS_CLIPCHILDREN.0) as isize,
        );
        let p_ex_style = GetWindowLongPtrW(target_hwnd, GWL_EXSTYLE) as u32;
        let _ = SetWindowLongPtrW(
            target_hwnd,
            GWL_EXSTYLE,
            (p_ex_style | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as isize,
        );

        let mut virt_w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let mut virt_h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        let virt_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let virt_y = GetSystemMetrics(SM_YVIRTUALSCREEN);

        if virt_w <= 0 || virt_h <= 0 {
            virt_w = GetSystemMetrics(SM_CXSCREEN);
            virt_h = GetSystemMetrics(SM_CYSCREEN);
        }

        let mut current_rect = windows::Win32::Foundation::RECT::default();
        let _ = GetWindowRect(target_hwnd, &mut current_rect);
        let curr_w = current_rect.right - current_rect.left;
        let curr_h = current_rect.bottom - current_rect.top;

        if curr_w < virt_w || curr_h < virt_h {
            let is_child_of_progman =
                !progman_current.0.is_null() && GetParent(target_hwnd) == Ok(progman_current);

            if is_child_of_progman {
                let _ = SetWindowPos(
                    target_hwnd,
                    None,
                    0,
                    0,
                    virt_w,
                    virt_h,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            } else {
                let _ = SetWindowPos(
                    target_hwnd,
                    None,
                    virt_x,
                    virt_y,
                    virt_w,
                    virt_h,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        } else {
            let _ = ShowWindow(target_hwnd, SW_SHOW);
        }
    }

    target_hwnd
}

/// Maps a monitor's virtual screen `DisplayRect` to `WorkerW` client `(x, y, width, height)` using
/// `MapWindowPoints`, falling back to `virtual_rect_to_client_coords` with `SM_XVIRTUALSCREEN`/`SM_YVIRTUALSCREEN`.
pub fn map_monitor_rect_to_workerw(workerw: HWND, rect: DisplayRect) -> (i32, i32, i32, i32) {
    if !workerw.0.is_null() {
        let mut pt = windows::Win32::Foundation::POINT {
            x: rect.left,
            y: rect.top,
        };
        unsafe {
            use windows::Win32::Foundation::{GetLastError, SetLastError, WIN32_ERROR};
            use windows::Win32::Graphics::Gdi::MapWindowPoints;

            SetLastError(WIN32_ERROR(0));
            let mapped = MapWindowPoints(None, Some(workerw), std::slice::from_mut(&mut pt));
            if mapped != 0 || GetLastError().0 == 0 {
                return (pt.x, pt.y, rect.width(), rect.height());
            }
        }
    }

    let origin_x = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
            windows::Win32::UI::WindowsAndMessaging::SM_XVIRTUALSCREEN,
        )
    };
    let origin_y = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
            windows::Win32::UI::WindowsAndMessaging::SM_YVIRTUALSCREEN,
        )
    };
    virtual_rect_to_client_coords(rect, origin_x, origin_y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayRect;

    #[test]
    fn test_workerw_hooking_function() {
        let progman = get_progman_hwnd();
        println!("Progman HWND: {:?}", progman);
        let workerw = attach_to_desktop();
        println!("WorkerW HWND: {:?}", workerw);
    }

    #[test]
    fn test_virtual_rect_to_client_coords_negative_origin() {
        let left_monitor = DisplayRect {
            left: -1920,
            top: -360,
            right: 0,
            bottom: 720,
        };
        let origin_x = -1920;
        let origin_y = -360;

        let (x, y, w, h) = virtual_rect_to_client_coords(left_monitor, origin_x, origin_y);
        assert_eq!((x, y, w, h), (0, 0, 1920, 1080));

        let right_monitor = DisplayRect {
            left: 0,
            top: 0,
            right: 3840,
            bottom: 2160,
        };
        let (rx, ry, rw, rh) = virtual_rect_to_client_coords(right_monitor, origin_x, origin_y);
        assert_eq!((rx, ry, rw, rh), (1920, 360, 3840, 2160));
    }

    #[test]
    fn test_map_monitor_rect_to_workerw_null_hwnd_fallback() {
        let monitor = DisplayRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        // Calling with null HWND should use SM_XVIRTUALSCREEN / SM_YVIRTUALSCREEN fallback without panic
        let (x, y, w, h) = map_monitor_rect_to_workerw(HWND::default(), monitor);
        assert_eq!(w, 1920);
        assert_eq!(h, 1080);
        let origin_x = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_XVIRTUALSCREEN,
            )
        };
        let origin_y = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_YVIRTUALSCREEN,
            )
        };
        assert_eq!(x, monitor.left - origin_x);
        assert_eq!(y, monitor.top - origin_y);
    }
}
