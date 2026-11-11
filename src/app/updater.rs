//! Asynchronous GitHub release update checker.

use super::log::{LogLevel, log_msg, log_with_level};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW,
};
use windows::core::HSTRING;

const GITHUB_RELEASES_API_URL: &str =
    "https://api.github.com/repos/ArtixForLiving/ArcPaper/releases/latest";

static IS_CHECKING_UPDATES: AtomicBool = AtomicBool::new(false);

struct UpdateCheckGuard;
impl Drop for UpdateCheckGuard {
    fn drop(&mut self) {
        IS_CHECKING_UPDATES.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub html_url: String,
}

/// Parses numeric components from a semantic version string (e.g. `"v1.2.3"` -> `[1, 2, 3]`).
pub fn parse_version_numbers(v: &str) -> Vec<u64> {
    let without_build = v.trim().split('+').next().unwrap_or("");
    let base = without_build.split('-').next().unwrap_or("").trim();
    let trimmed = base.trim_start_matches(['v', 'V']);
    trimmed
        .split('.')
        .filter_map(|num_str| num_str.parse::<u64>().ok())
        .collect()
}

/// Returns `true` if `latest_tag` is newer than `current_ver`.
pub fn is_newer_release(latest_tag: &str, current_ver: &str) -> bool {
    let latest_nums = parse_version_numbers(latest_tag);
    let current_nums = parse_version_numbers(current_ver);

    if !latest_nums.is_empty() && !current_nums.is_empty() {
        latest_nums > current_nums
    } else {
        let l_clean = latest_tag.trim().trim_start_matches(['v', 'V']);
        let c_clean = current_ver.trim().trim_start_matches(['v', 'V']);
        l_clean != c_clean
    }
}

fn show_update_dialog_failed() {
    let title = HSTRING::from("ArcPaper");
    let message = HSTRING::from(
        "Failed to check for updates. Please check your internet connection and try again.",
    );
    unsafe {
        MessageBoxW(None, &message, &title, MB_ICONWARNING | MB_OK);
    }
}

/// Queries the GitHub Releases API and notifies the user with a native dialog.
pub fn check_for_updates() {
    if IS_CHECKING_UPDATES
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        log_msg("[ArcPaper] Update check already in progress. Ignoring duplicate request.");
        return;
    }
    let _guard = UpdateCheckGuard;

    log_msg("[ArcPaper] Checking for updates from GitHub releases...");
    let user_agent = format!("ArcPaper/{}", env!("CARGO_PKG_VERSION"));
    let response = ureq::get(GITHUB_RELEASES_API_URL)
        .set("User-Agent", &user_agent)
        .timeout(Duration::from_secs(10))
        .call();

    let release: GitHubRelease = match response {
        Ok(resp) => match resp.into_json() {
            Ok(data) => data,
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!("[ArcPaper] Failed to parse release response JSON: {:?}", e),
                );
                show_update_dialog_failed();
                return;
            }
        },
        Err(e) => {
            log_with_level(
                LogLevel::Warn,
                &format!("[ArcPaper] Failed to fetch release from GitHub API: {:?}", e),
            );
            show_update_dialog_failed();
            return;
        }
    };

    let current_version = env!("CARGO_PKG_VERSION");
    log_msg(&format!(
        "[ArcPaper] Current version: {}, Latest release tag: {}",
        current_version, release.tag_name
    ));

    if is_newer_release(&release.tag_name, current_version) {
        log_msg(&format!(
            "[ArcPaper] New update available: {} -> {}",
            current_version, release.tag_name
        ));
        let title = HSTRING::from("ArcPaper Update Available");
        let message = HSTRING::from(format!(
            "A new version ({}) is available!\n\nCurrent version: {}\n\nWould you like to open the GitHub Release page to download it?",
            release.tag_name, current_version
        ));
        let result = unsafe { MessageBoxW(None, &message, &title, MB_ICONINFORMATION | MB_YESNO) };
        if result == IDYES {
            log_msg(&format!(
                "[ArcPaper] User confirmed update. Opening URL: {}",
                release.html_url
            ));
            let _ = std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", &release.html_url])
                .spawn();
        }
    } else {
        log_msg(&format!(
            "[ArcPaper] Application is up to date ({})",
            current_version
        ));
        let title = HSTRING::from("ArcPaper");
        let message = HSTRING::from(format!(
            "You are already using the latest version of ArcPaper ({}).",
            current_version
        ));
        unsafe {
            MessageBoxW(None, &message, &title, MB_ICONINFORMATION | MB_OK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_parsing_and_comparison() {
        assert_eq!(parse_version_numbers("v1.2.3"), vec![1, 2, 3]);
        assert_eq!(parse_version_numbers("0.1.0"), vec![0, 1, 0]);
        assert_eq!(parse_version_numbers("v0.1.0-alpha"), vec![0, 1, 0]);
        assert_eq!(parse_version_numbers("  v2.0.0-rc.1 "), vec![2, 0, 0]);

        assert!(is_newer_release("v0.2.0", "0.1.0"));
        assert!(is_newer_release("v1.0.0", "0.1.0"));
        assert!(is_newer_release("v0.1.1", "0.1.0"));
        assert!(is_newer_release("0.2.0", "0.1.0"));

        assert!(!is_newer_release("v0.1.0", "0.1.0"));
        assert!(!is_newer_release("0.1.0", "0.1.0"));
        assert!(!is_newer_release("v0.0.9", "0.1.0"));
        assert!(!is_newer_release("v0.1.0-beta", "0.1.0"));
    }

    #[test]
    fn test_github_release_json_deserialization() {
        let sample_json = r#"{
            "tag_name": "v1.5.0",
            "html_url": "https://github.com/ArtixForLiving/ArcPaper/releases/tag/v1.5.0",
            "name": "ArcPaper v1.5.0 Release"
        }"#;
        let release: GitHubRelease =
            serde_json::from_str(sample_json).expect("Failed to deserialize sample GitHub release");
        assert_eq!(release.tag_name, "v1.5.0");
        assert_eq!(
            release.html_url,
            "https://github.com/ArtixForLiving/ArcPaper/releases/tag/v1.5.0"
        );
    }
}
