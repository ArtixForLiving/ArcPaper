//! Portable executable, configuration, and wallpaper directory resolution.

use std::path::{Path, PathBuf};

/// Resolves the primary executable path, avoiding Cargo test runner binaries inside `deps/`.
pub fn resolve_main_exe_from(current_exe: Option<&Path>) -> PathBuf {
    if let Some(exe) = current_exe {
        let is_deps = exe.components().any(|c| c.as_os_str() == "deps");
        if is_deps {
            if let Some(deps_parent) = exe.parent().and_then(|p| p.parent()) {
                let sibling_exe = deps_parent.join("arc_paper.exe");
                if sibling_exe.exists() {
                    return sibling_exe;
                }
            }
        } else {
            return exe.to_path_buf();
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("arc_paper.exe")
}

/// Resolves the application base directory from the running executable (or `cwd` under test runners).
pub fn resolve_app_dir_from(current_exe: Option<&Path>, cwd: Option<&Path>) -> PathBuf {
    if let Some(exe) = current_exe {
        let is_deps = exe.components().any(|c| c.as_os_str() == "deps");
        if !is_deps && let Some(parent) = exe.parent() {
            return parent.to_path_buf();
        }
    }
    cwd.map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn get_main_exe_path() -> PathBuf {
    resolve_main_exe_from(std::env::current_exe().ok().as_deref())
}

pub fn get_app_dir() -> PathBuf {
    resolve_app_dir_from(
        std::env::current_exe().ok().as_deref(),
        std::env::current_dir().ok().as_deref(),
    )
}

pub fn get_saved_wallpapers_dir() -> PathBuf {
    let wp_dir = get_app_dir().join("SavedWallpapers");
    let _ = std::fs::create_dir_all(&wp_dir);
    wp_dir
}

pub fn get_config_path() -> PathBuf {
    get_app_dir().join("config.json")
}

pub fn get_app_log_path() -> PathBuf {
    get_app_dir().join("app.log")
}

/// Sets working directory, DLL search path, and Per-Monitor V2 DPI awareness for the main process.
#[cfg(windows)]
pub fn configure_process_and_dpi() {
    let main_exe = get_main_exe_path();
    if let Some(exe_dir) = main_exe.parent() {
        let _ = std::env::set_current_dir(exe_dir);
        unsafe {
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::System::LibraryLoader::SetDllDirectoryW;
            use windows::core::PCWSTR;

            let wide_exe_dir: Vec<u16> = exe_dir
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let _ = SetDllDirectoryW(PCWSTR(wide_exe_dir.as_ptr()));
        }
    }

    unsafe {
        use windows::Win32::UI::HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
        };
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err() {
            use windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware;
            let _ = SetProcessDPIAware();
        }

        use windows::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
        };
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}

/// Acquires the application single-instance mutex, terminating the process cleanly if another instance is running.
#[cfg(windows)]
pub fn acquire_single_instance_mutex_or_exit() -> Option<windows::Win32::Foundation::HANDLE> {
    unsafe {
        use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::core::w;

        let handle = CreateMutexW(None, true, w!("ArcPaper_SingleInstance_Mutex"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            crate::app::log::log_msg(
                "[ArcPaper] Another instance of ArcPaper is already running. Exiting cleanly.",
            );
            std::process::exit(0);
        }
        handle.ok()
    }
}

/// Returns a sorted list of `.mp4` filenames present directly inside `dir`.
pub fn list_mp4_files(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension().and_then(|s| s.to_str())
                && ext.eq_ignore_ascii_case("mp4")
                && let Some(name) = path.file_name().and_then(|s| s.to_str())
            {
                files.push(name.to_string());
            }
        }
    }
    files.sort();
    files
}

/// Command-line flags parsed at application startup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CliArgs {
    /// Enables auto-pause transition latency diagnostic logging.
    pub diag_pause: bool,
    /// Forces window monitor to run in legacy 1000 ms polling mode.
    pub force_polling: bool,
}

impl CliArgs {
    /// Parses flags from `std::env::args()`.
    pub fn parse() -> Self {
        Self::parse_from(std::env::args())
    }

    /// Parses flags from any iterator of argument strings.
    pub fn parse_from<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let mut cli = Self::default();
        for arg in args {
            match arg.as_ref() {
                "--diag-pause" => cli.diag_pause = true,
                "--force-polling" => cli.force_polling = true,
                _ => {}
            }
        }
        cli
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_args_parsing() {
        let empty: Vec<&str> = vec![];
        assert_eq!(CliArgs::parse_from(empty), CliArgs::default());

        let diag = vec!["arc_paper.exe", "--diag-pause"];
        assert_eq!(
            CliArgs::parse_from(diag),
            CliArgs {
                diag_pause: true,
                force_polling: false,
            }
        );

        let poll = vec!["arc_paper.exe", "--force-polling"];
        assert_eq!(
            CliArgs::parse_from(poll),
            CliArgs {
                diag_pause: false,
                force_polling: true,
            }
        );

        let both = vec![
            "arc_paper.exe",
            "--diag-pause",
            "--force-polling",
            "--unknown",
        ];
        assert_eq!(
            CliArgs::parse_from(both),
            CliArgs {
                diag_pause: true,
                force_polling: true,
            }
        );
    }

    #[test]
    fn test_resolve_paths_from_portable_exe_and_deps_runner() {
        let portable_exe = PathBuf::from(r"C:\Tools\ArcPaper\arc_paper.exe");
        assert_eq!(resolve_main_exe_from(Some(&portable_exe)), portable_exe);

        let dir = std::env::temp_dir().join(format!("arc_paper_paths_{}", std::process::id()));
        let deps_dir = dir.join("deps");
        let _ = std::fs::create_dir_all(&deps_dir);
        let sibling_exe = dir.join("arc_paper.exe");
        std::fs::write(&sibling_exe, b"").unwrap();
        let deps_exe = deps_dir.join("arc_paper-abc123.exe");

        let legacy_dir = concat!("Arc", " Paper");
        let resolved_from_deps = resolve_main_exe_from(Some(&deps_exe));
        assert_eq!(resolved_from_deps, sibling_exe);
        assert!(!resolved_from_deps.to_string_lossy().contains(legacy_dir));

        let app_dir = resolve_app_dir_from(Some(&portable_exe), Some(&dir));
        assert_eq!(app_dir, PathBuf::from(r"C:\Tools\ArcPaper"));
        assert!(!app_dir.to_string_lossy().contains(legacy_dir));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
