//! Windows auto-start registry integration under HKCU Run.

use super::log::log_msg;
use super::paths::get_main_exe_path;

pub const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const APP_REG_NAME: &str = "ArcPaper";

/// Checks whether ArcPaper is registered in the current user's Run registry key.
pub fn is_autostart_enabled() -> bool {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    if let Ok(run_key) = hkcu.open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_READ) {
        let val: Result<String, _> = run_key.get_value(APP_REG_NAME);
        val.is_ok()
    } else {
        false
    }
}

/// Registers the current executable path in the HKCU Run key wrapped in double quotes.
pub fn enable_autostart() -> Result<(), Box<dyn std::error::Error>> {
    let main_exe = get_main_exe_path();
    let exe_path_str = main_exe.to_string_lossy().to_string();
    let quoted_path = format!("\"{}\"", exe_path_str);

    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let (run_key, _) = hkcu.create_subkey(RUN_KEY_PATH)?;
    run_key.set_value(APP_REG_NAME, &quoted_path)?;
    log_msg(&format!(
        "[ArcPaper] Registered auto-start in registry: {}",
        quoted_path
    ));
    Ok(())
}

/// Removes the ArcPaper entry from the HKCU Run key.
pub fn disable_autostart() -> Result<(), Box<dyn std::error::Error>> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    if let Ok(run_key) = hkcu.open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_WRITE) {
        let _ = run_key.delete_value(APP_REG_NAME);
        log_msg("[ArcPaper] Unregistered auto-start from registry.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autostart_registry_cycle() {
        let initial = is_autostart_enabled();

        assert!(enable_autostart().is_ok());
        assert!(is_autostart_enabled());

        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        if let Ok(run_key) = hkcu.open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_READ) {
            let val: String = run_key.get_value(APP_REG_NAME).unwrap();
            assert!(val.starts_with('"') && val.ends_with('"'));
            assert!(!val.contains("deps"));
        }

        assert!(disable_autostart().is_ok());
        assert!(!is_autostart_enabled());

        if initial {
            let _ = enable_autostart();
        }
    }
}
