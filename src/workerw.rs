use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, SMTO_NORMAL, SendMessageTimeoutW,
};
use windows::core::{PCWSTR, w};

/// Attempts to attach the current thread to the interactive "Default" desktop
/// if the process is running in a secondary window station or sandbox environment.
pub fn attach_to_default_desktop() -> bool {
    type FnOpenDesktopW = unsafe extern "system" fn(*const u16, u32, i32, u32) -> isize;
    type FnSetThreadDesktop = unsafe extern "system" fn(isize) -> i32;

    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        if let Ok(user32) = LoadLibraryW(w!("user32.dll")) {
            let p_open = GetProcAddress(user32, windows::core::s!("OpenDesktopW"));
            let p_set = GetProcAddress(user32, windows::core::s!("SetThreadDesktop"));
            if let (Some(open_fn), Some(set_fn)) = (p_open, p_set) {
                let open_desktop: FnOpenDesktopW = std::mem::transmute(open_fn);
                let set_thread_desktop: FnSetThreadDesktop = std::mem::transmute(set_fn);
                let name: Vec<u16> = "Default\0".encode_utf16().collect();
                // 0x01FF covers standard DESKTOP rights
                let hdesk = open_desktop(name.as_ptr(), 0, 0, 0x01FF);
                if hdesk != 0 {
                    return set_thread_desktop(hdesk) != 0;
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

/// Finds the `SHELLDLL_DefView` window handle (desktop icon view),
/// which may be a child of `Progman` or a child of a top-level `WorkerW`.
#[allow(dead_code)]
pub fn get_shelldll_defview_hwnd(progman: HWND) -> HWND {
    unsafe {
        // 1. Search inside Progman (typical in Windows 11 24H2 / 25H2)
        if !progman.0.is_null() {
            if let Ok(h) =
                FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), PCWSTR::null())
            {
                if !h.0.is_null() {
                    return h;
                }
            }
        }

        // 2. Fallback search: iterate top-level WorkerW windows
        let mut top_worker =
            FindWindowExW(None, None, w!("WorkerW"), PCWSTR::null()).unwrap_or(HWND::default());
        while !top_worker.0.is_null() {
            if let Ok(child_shell) = FindWindowExW(
                Some(top_worker),
                None,
                w!("SHELLDLL_DefView"),
                PCWSTR::null(),
            ) {
                if !child_shell.0.is_null() {
                    return child_shell;
                }
            }
            top_worker = FindWindowExW(None, Some(top_worker), w!("WorkerW"), PCWSTR::null())
                .unwrap_or(HWND::default());
        }

        HWND::default()
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
        {
            if !worker_in_progman.0.is_null() {
                return worker_in_progman;
            }
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
                if attempt > 1 {
                    println!(
                        "[ArcPaper] Desktop WorkerW successfully attached on attempt {}",
                        attempt
                    );
                }
                return workerw;
            }
        }

        let fallback_workerw = get_desktop_workerw_hwnd_internal();
        if !fallback_workerw.0.is_null() {
            if attempt > 1 {
                println!(
                    "[ArcPaper] Desktop WorkerW successfully attached on attempt {}",
                    attempt
                );
            }
            return fallback_workerw;
        }

        if attempt < 10 {
            println!(
                "[ArcPaper] Desktop shell (Progman/WorkerW) not ready (attempt {}/10). Retrying in 500ms...",
                attempt
            );
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

/// Retrieves the desktop WorkerW window handle using retry logic.
#[allow(dead_code)]
pub fn get_desktop_workerw_hwnd() -> HWND {
    attach_to_desktop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workerw_hooking_function() {
        let progman = get_progman_hwnd();
        println!("Progman HWND: {:?}", progman);
        let workerw = get_desktop_workerw_hwnd();
        println!("WorkerW HWND: {:?}", workerw);
    }
}
