//! Clean uninstallation handler removing startup entries and restoring desktop wallpaper.

use super::autostart::disable_autostart;
use super::log::{LogLevel, log_msg, log_with_level};
use super::paths::{get_config_path, get_main_exe_path};
use std::path::Path;
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW, SPI_SETDESKWALLPAPER,
    SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SystemParametersInfoW,
};
use windows::core::HSTRING;

pub fn restore_default_wallpaper() {
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(std::ptr::null_mut()),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        );
    }
}

pub fn format_uninstall_command(exe_path: &Path, config_path: &Path) -> String {
    format!(
        "ping 127.0.0.1 -n 3 > nul & del /F /Q \"{}\" \"{}\"",
        exe_path.display(),
        config_path.display()
    )
}

/// Uninstalls ArcPaper by removing startup registry entries, restoring the default desktop wallpaper,
/// launching a background self-deletion script, and exiting cleanly.
pub fn perform_uninstall() {
    let title = HSTRING::from("Uninstall ArcPaper");
    let confirm_msg = HSTRING::from(
        "Are you sure you want to uninstall ArcPaper?\n\nThis will remove all startup registry entries, restore your default desktop wallpaper, and delete application binaries.",
    );
    let result = unsafe { MessageBoxW(None, &confirm_msg, &title, MB_ICONWARNING | MB_YESNO) };
    if result != IDYES {
        log_msg("[ArcPaper] Uninstallation cancelled by user.");
        return;
    }

    log_msg("[ArcPaper] User confirmed uninstallation. Cleaning up system resources...");

    if let Err(e) = disable_autostart() {
        log_with_level(
            LogLevel::Warn,
            &format!(
                "[ArcPaper] Warning: Failed to remove startup registry entry: {:?}",
                e
            ),
        );
    }

    restore_default_wallpaper();

    let exe_path = get_main_exe_path();
    let config_path = get_config_path();
    let cmd_str = format_uninstall_command(&exe_path, &config_path);

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let _ = std::process::Command::new("cmd.exe")
        .args(["/C", &cmd_str])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();

    let done_msg = HSTRING::from(
        "ArcPaper has been successfully uninstalled. Registry entries have been removed.",
    );
    unsafe {
        MessageBoxW(None, &done_msg, &title, MB_ICONINFORMATION | MB_OK);
    }

    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_uninstall_cmd_formatting() {
        let exe = PathBuf::from(r"C:\App\arc_paper.exe");
        let cfg = PathBuf::from(r"C:\App\config.json");
        let cmd = format_uninstall_command(&exe, &cfg);
        assert!(cmd.starts_with("ping 127.0.0.1 -n 3 > nul"));
        assert!(cmd.contains(r#"del /F /Q "C:\App\arc_paper.exe" "C:\App\config.json""#));
    }
}
