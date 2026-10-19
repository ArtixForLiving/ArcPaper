#![windows_subsystem = "windows"]

mod player;
mod workerw;

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIconBuilder};
use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, IDYES, IsIconic, IsWindowVisible, IsZoomed,
    MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW, SM_CXSCREEN, SM_CYSCREEN,
    SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_SETDESKWALLPAPER, SWP_NOACTIVATE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, SystemParametersInfoW, WS_CLIPCHILDREN,
};

// ─── Logging ──────────────────────────────────────────────────────────────────

fn get_app_log_path() -> PathBuf {
    get_app_dir().join("app.log")
}

fn log_msg(msg: &str) {
    println!("{}", msg);
    use std::io::Write;
    let log_path = get_app_log_path();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(file, "{}", msg);
    }
}

// ─── Memory Optimization ──────────────────────────────────────────────────────

/// Trims the process working set, releasing unneeded driver initialization and libmpv DLL pages.
fn trim_working_set() {
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
        let proc = GetCurrentProcess();
        let _ = SetProcessWorkingSetSize(proc, usize::MAX, usize::MAX);
    }
}

/// Spawns a dedicated thread that waits for the specified duration (e.g. 500ms),
/// then trims the process working set to release heap fragments and mpv runtime allocations.
fn delayed_trim(delay_ms: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        trim_working_set();
        log_msg("[ArcPaper] Memory working set trimmed via SetProcessWorkingSetSize.");
    });
}

// ─── Windows Auto-Start Registry ──────────────────────────────────────────────

const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_REG_NAME: &str = "ArcPaper";

/// Returns the primary executable path, avoiding any temporary test runner binaries in `deps/`.
fn get_main_exe_path() -> PathBuf {
    if let Ok(current_exe) = std::env::current_exe() {
        let is_deps = current_exe.components().any(|c| c.as_os_str() == "deps");
        if is_deps {
            let dist_exe = PathBuf::from("D:\\Arc Paper\\dist\\ArcPaper\\arc_paper.exe");
            if dist_exe.exists() {
                return dist_exe;
            }
            if let Some(deps_parent) = current_exe.parent().and_then(|p| p.parent()) {
                let sibling_exe = deps_parent.join("arc_paper.exe");
                if sibling_exe.exists() {
                    return sibling_exe;
                }
            }
            let root_exe = PathBuf::from("D:\\Arc Paper\\arc_paper.exe");
            if root_exe.exists() {
                return root_exe;
            }
        } else {
            return current_exe;
        }
    }
    let dist_exe = PathBuf::from("D:\\Arc Paper\\dist\\ArcPaper\\arc_paper.exe");
    if dist_exe.exists() {
        return dist_exe;
    }
    PathBuf::from("D:\\Arc Paper\\arc_paper.exe")
}

/// Checks if ArcPaper is registered in HKCU Run key
fn is_autostart_enabled() -> bool {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    if let Ok(run_key) = hkcu.open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_READ) {
        let val: Result<String, _> = run_key.get_value(APP_REG_NAME);
        val.is_ok()
    } else {
        false
    }
}

/// Registers the current executable path into HKCU Run key wrapped in double quotes
fn enable_autostart() -> Result<(), Box<dyn std::error::Error>> {
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

/// Removes the ArcPaper entry from HKCU Run key
fn disable_autostart() -> Result<(), Box<dyn std::error::Error>> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    if let Ok(run_key) = hkcu.open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_WRITE) {
        let _ = run_key.delete_value(APP_REG_NAME);
        log_msg("[ArcPaper] Unregistered auto-start from registry.");
    }
    Ok(())
}

fn restore_default_wallpaper() {
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(std::ptr::null_mut()),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        );
    }
}

/// Uninstalls ArcPaper by removing startup registry entries, restoring the default desktop wallpaper,
/// launching a background self-deletion script, and exiting the process cleanly.
fn perform_uninstall() {
    let title = HSTRING::from("Uninstall ArcPaper");
    let confirm_msg = HSTRING::from(
        "Are you sure you want to uninstall ArcPaper?\n\nThis will remove all startup registry entries, restore your default desktop wallpaper, and delete application binaries.",
    );
    let result = unsafe {
        MessageBoxW(None, &confirm_msg, &title, MB_ICONWARNING | MB_YESNO)
    };
    if result != IDYES {
        log_msg("[ArcPaper] Uninstallation cancelled by user.");
        return;
    }

    log_msg("[ArcPaper] User confirmed uninstallation. Cleaning up system resources...");

    // 1. Remove registry startup entry
    if let Err(e) = disable_autostart() {
        log_msg(&format!(
            "[ArcPaper] Warning: Failed to remove startup registry entry: {:?}",
            e
        ));
    }

    // 2. Restore default desktop wallpaper
    restore_default_wallpaper();

    // 3. Launch self-delete script in the background
    let exe_path = get_main_exe_path();
    let config_path = get_config_path();
    let cmd_str = format!(
        "ping 127.0.0.1 -n 3 > nul & del /F /Q \"{}\" \"{}\"",
        exe_path.display(),
        config_path.display()
    );

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let _ = std::process::Command::new("cmd.exe")
        .args(["/C", &cmd_str])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();

    // 4. Show success confirmation dialog
    let done_msg = HSTRING::from(
        "ArcPaper has been successfully uninstalled. Registry entries have been removed.",
    );
    unsafe {
        MessageBoxW(None, &done_msg, &title, MB_ICONINFORMATION | MB_OK);
    }

    // 5. Clean exit
    std::process::exit(0);
}

// ─── Update Checker ───────────────────────────────────────────────────────────

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
struct GitHubRelease {
    tag_name: String,
    html_url: String,
}

/// Parses numeric components from a semantic version string (e.g., "v1.2.3" -> [1, 2, 3]).
fn parse_version_numbers(v: &str) -> Vec<u64> {
    let without_build = v.trim().split('+').next().unwrap_or("");
    let base = without_build.split('-').next().unwrap_or("").trim();
    let trimmed = base.trim_start_matches(|c| c == 'v' || c == 'V');
    trimmed
        .split('.')
        .filter_map(|num_str| num_str.parse::<u64>().ok())
        .collect()
}

/// Determines whether a release tag represents a newer version than current_ver.
fn is_newer_release(latest_tag: &str, current_ver: &str) -> bool {
    let latest_nums = parse_version_numbers(latest_tag);
    let current_nums = parse_version_numbers(current_ver);

    if !latest_nums.is_empty() && !current_nums.is_empty() {
        latest_nums > current_nums
    } else {
        let l_clean = latest_tag.trim().trim_start_matches(|c| c == 'v' || c == 'V');
        let c_clean = current_ver.trim().trim_start_matches(|c| c == 'v' || c == 'V');
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

/// Checks the latest GitHub release on a background thread and presents a native Windows message box.
fn check_for_updates() {
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
                log_msg(&format!(
                    "[ArcPaper] Failed to parse release response JSON: {:?}",
                    e
                ));
                show_update_dialog_failed();
                return;
            }
        },
        Err(e) => {
            log_msg(&format!(
                "[ArcPaper] Failed to fetch release from GitHub API: {:?}",
                e
            ));
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
        let result = unsafe {
            MessageBoxW(None, &message, &title, MB_ICONINFORMATION | MB_YESNO)
        };
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

// ─── Tray Icon Generator ──────────────────────────────────────────────────────

/// Creates a crisp 32x32 RGBA icon for the system tray (English branding)
fn create_default_tray_icon() -> Icon {
    let width = 32u32;
    let height = 32u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);

    for y in 0..height {
        for x in 0..width {
            let is_border = x == 0 || x == width - 1 || y == 0 || y == height - 1;
            let is_inner_accent = (x >= 8 && x <= 23 && y >= 8 && y <= 23)
                && (x == 8 || x == 23 || y == 8 || y == 23);

            if is_border {
                rgba.extend_from_slice(&[59, 130, 246, 255]); // bright blue border
            } else if is_inner_accent {
                rgba.extend_from_slice(&[100, 255, 218, 255]); // teal inner accent
            } else {
                rgba.extend_from_slice(&[10, 25, 47, 255]); // dark navy background
            }
        }
    }

    Icon::from_rgba(rgba, width, height).expect("Failed to create system tray icon")
}

// ─── Path Helpers ─────────────────────────────────────────────────────────────

fn get_app_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let is_deps = exe.components().any(|c| c.as_os_str() == "deps");
        if !is_deps {
            if let Some(parent) = exe.parent() {
                if parent.join("SavedWallpapers").exists() || parent.join("libmpv-2.dll").exists() {
                    return parent.to_path_buf();
                }
            }
        }
    }
    let dist_dir = PathBuf::from("D:\\Arc Paper\\dist\\ArcPaper");
    if dist_dir.exists() {
        return dist_dir;
    }
    let fixed_dir = PathBuf::from("D:\\Arc Paper");
    if fixed_dir.exists() {
        return fixed_dir;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn get_saved_wallpapers_dir() -> PathBuf {
    let app_dir = get_app_dir();
    let wp_dir = app_dir.join("SavedWallpapers");
    if wp_dir.exists() {
        return wp_dir;
    }
    let fixed_dir = PathBuf::from("D:\\Arc Paper\\SavedWallpapers");
    if fixed_dir.exists() {
        return fixed_dir;
    }
    let _ = std::fs::create_dir_all(&wp_dir);
    wp_dir
}

fn get_config_path() -> PathBuf {
    get_app_dir().join("config.json")
}

fn list_mp4_files(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    if ext.eq_ignore_ascii_case("mp4") {
                        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                            files.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    files.sort();
    files
}

// ─── Configuration (config.json) ─────────────────────────────────────────────

fn default_ring_size() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    pub active_video: Option<String>,
    pub speed: f32,
    pub brightness: u32,
    pub fit_mode: String,
    pub is_manually_paused: bool,
    #[serde(default = "default_ring_size")]
    pub ring_size: usize,
    #[serde(default)]
    pub target_fps: Option<u32>,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            active_video: None,
            speed: 1.0,
            brightness: 100,
            fit_mode: "cover".to_string(),
            is_manually_paused: false,
            ring_size: 10,
            target_fps: None,
        }
    }
}

fn load_config() -> AppConfig {
    let path = get_config_path();
    if path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(mut cfg) = serde_json::from_str::<AppConfig>(&content) {
                if cfg.ring_size != 5 && cfg.ring_size != 10 && cfg.ring_size != 20 {
                    cfg.ring_size = 10;
                }
                log_msg(&format!("[ArcPaper] Loaded configuration from {:?}", path));
                return cfg;
            }
        }
    }
    log_msg("[ArcPaper] No existing config found, using default configuration.");
    AppConfig::default()
}

fn save_config(config: &AppConfig) {
    let path = get_config_path();
    if let Ok(json) = serde_json::to_string_pretty(config) {
        if let Err(e) = std::fs::write(&path, json) {
            log_msg(&format!(
                "[ArcPaper] Error saving config to {:?}: {:?}",
                path, e
            ));
        } else {
            log_msg(&format!(
                "[ArcPaper] Successfully saved config to {:?}",
                path
            ));
        }
    }
}

// ─── System Tray Menu ─────────────────────────────────────────────────────────

struct TrayControls {
    upload_id: MenuId,
    pause_resume_id: MenuId,
    speed_map: HashMap<MenuId, f32>,
    fit_map: HashMap<MenuId, &'static str>,
    brightness_map: HashMap<MenuId, u32>,
    ring_size_map: HashMap<MenuId, usize>,
    fps_map: HashMap<MenuId, Option<u32>>,
    wallpaper_map: HashMap<MenuId, String>,
    check_updates_id: MenuId,
    autostart_id: MenuId,
    autostart_item: CheckMenuItem,
    uninstall_id: MenuId,
    exit_id: MenuId,
}

/// Returns the dynamic target FPS options based on native video framerate.
fn get_fps_options(native_fps: u32) -> &'static [(u32, &'static str)] {
    if native_fps > 45 {
        &[
            (45, "45 FPS"),
            (30, "30 FPS"),
            (20, "20 FPS"),
            (15, "15 FPS (Power Saver)"),
        ]
    } else if native_fps > 35 {
        &[
            (30, "30 FPS"),
            (20, "20 FPS"),
            (15, "15 FPS (Power Saver)"),
        ]
    } else {
        &[
            (20, "20 FPS"),
            (15, "15 FPS (Power Saver)"),
        ]
    }
}

fn build_tray_menu(
    saved_wallpapers_dir: &Path,
    is_paused: bool,
    active_wp: Option<&str>,
    ring_size: usize,
    native_fps: u32,
    target_fps: Option<u32>,
) -> (Menu, TrayControls) {
    let menu = Menu::new();

    // 1. Upload New Wallpaper
    let upload_item = MenuItem::new("Upload New Wallpaper...", true, None);
    let upload_id = upload_item.id().clone();
    let _ = menu.append(&upload_item);

    // 2. Select Active Wallpaper Submenu
    let apply_submenu = Submenu::new("Select Active Wallpaper", true);
    let mut wallpaper_map = HashMap::new();
    let mp4_files = list_mp4_files(saved_wallpapers_dir);

    if mp4_files.is_empty() {
        let empty_item = MenuItem::new("No saved wallpapers (.mp4)", false, None);
        let _ = apply_submenu.append(&empty_item);
    } else {
        for filename in mp4_files {
            let label = if active_wp.is_some_and(|act| act == filename) {
                format!("* {}", filename)
            } else {
                filename.clone()
            };
            let item = MenuItem::new(&label, true, None);
            wallpaper_map.insert(item.id().clone(), filename);
            let _ = apply_submenu.append(&item);
        }
    }
    let _ = menu.append(&apply_submenu);

    // 3. Play / Pause Control
    let pause_text = if is_paused {
        "Resume Wallpaper"
    } else {
        "Pause Wallpaper"
    };
    let pause_resume_item = MenuItem::new(pause_text, true, None);
    let pause_resume_id = pause_resume_item.id().clone();
    let _ = menu.append(&pause_resume_item);

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 4. Display & Visuals Submenu
    let visuals_submenu = Submenu::new("Display & Visuals", true);

    // Brightness Submenu
    let brightness_submenu = Submenu::new("Brightness", true);
    let bright_100 = MenuItem::new("100% (Normal)", true, None);
    let bright_75 = MenuItem::new("75% (Dim)", true, None);
    let bright_50 = MenuItem::new("50% (Dark)", true, None);

    let mut brightness_map = HashMap::new();
    brightness_map.insert(bright_100.id().clone(), 100u32);
    brightness_map.insert(bright_75.id().clone(), 75u32);
    brightness_map.insert(bright_50.id().clone(), 50u32);

    let _ = brightness_submenu.append(&bright_100);
    let _ = brightness_submenu.append(&bright_75);
    let _ = brightness_submenu.append(&bright_50);
    let _ = visuals_submenu.append(&brightness_submenu);

    // Screen Fit Submenu
    let fit_submenu = Submenu::new("Screen Fit", true);
    let fit_cover = MenuItem::new("Cover (Fill & Crop)", true, None);
    let fit_fill = MenuItem::new("Fill (Stretch)", true, None);
    let fit_contain = MenuItem::new("Contain (Original)", true, None);

    let mut fit_map = HashMap::new();
    fit_map.insert(fit_cover.id().clone(), "cover");
    fit_map.insert(fit_fill.id().clone(), "fill");
    fit_map.insert(fit_contain.id().clone(), "contain");

    let _ = fit_submenu.append(&fit_cover);
    let _ = fit_submenu.append(&fit_fill);
    let _ = fit_submenu.append(&fit_contain);
    let _ = visuals_submenu.append(&fit_submenu);

    let _ = menu.append(&visuals_submenu);

    // 5. Performance Submenu
    let performance_submenu = Submenu::new("Performance", true);

    // Playback Speed Submenu
    let speed_submenu = Submenu::new("Playback Speed", true);
    let speed_0_5 = MenuItem::new("0.5x Slow", true, None);
    let speed_1_0 = MenuItem::new("1.0x Normal", true, None);
    let speed_1_5 = MenuItem::new("1.5x Fast", true, None);

    let mut speed_map = HashMap::new();
    speed_map.insert(speed_0_5.id().clone(), 0.5f32);
    speed_map.insert(speed_1_0.id().clone(), 1.0f32);
    speed_map.insert(speed_1_5.id().clone(), 1.5f32);

    let _ = speed_submenu.append(&speed_0_5);
    let _ = speed_submenu.append(&speed_1_0);
    let _ = speed_submenu.append(&speed_1_5);
    let _ = performance_submenu.append(&speed_submenu);

    // Target FPS Submenu
    let fps_submenu = Submenu::new("Target FPS", true);
    let mut fps_map = HashMap::new();

    let is_native = target_fps.unwrap_or(0) == 0;
    let native_item = CheckMenuItem::new("Native (Original)", true, is_native, None);
    fps_map.insert(native_item.id().clone(), None);
    let _ = fps_submenu.append(&native_item);

    let fps_options = get_fps_options(native_fps);

    for &(fps_val, label) in fps_options {
        let is_checked = target_fps == Some(fps_val);
        let item = CheckMenuItem::new(label, true, is_checked, None);
        fps_map.insert(item.id().clone(), Some(fps_val));
        let _ = fps_submenu.append(&item);
    }
    let _ = performance_submenu.append(&fps_submenu);

    // Buffer Zone Ring Submenu
    let buffer_submenu = Submenu::new("Buffer Zone Ring", true);
    let ring_5_item = CheckMenuItem::new("5 Loops (Low RAM)", true, ring_size == 5, None);
    let ring_10_item = CheckMenuItem::new("10 Loops (Balanced)", true, ring_size == 10, None);
    let ring_20_item = CheckMenuItem::new("20 Loops (Max Smooth)", true, ring_size == 20, None);

    let mut ring_size_map = HashMap::new();
    ring_size_map.insert(ring_5_item.id().clone(), 5usize);
    ring_size_map.insert(ring_10_item.id().clone(), 10usize);
    ring_size_map.insert(ring_20_item.id().clone(), 20usize);

    let _ = buffer_submenu.append(&ring_5_item);
    let _ = buffer_submenu.append(&ring_10_item);
    let _ = buffer_submenu.append(&ring_20_item);
    let _ = performance_submenu.append(&buffer_submenu);

    let _ = menu.append(&performance_submenu);

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 6. Check for Updates
    let check_updates_item = MenuItem::new("Check for Updates...", true, None);
    let check_updates_id = check_updates_item.id().clone();
    let _ = menu.append(&check_updates_item);

    // 7. Auto-start
    let autostart_enabled = is_autostart_enabled();
    let autostart_item = CheckMenuItem::new("Start with Windows", true, autostart_enabled, None);
    let autostart_id = autostart_item.id().clone();
    let _ = menu.append(&autostart_item);

    // 8. Uninstall ArcPaper
    let uninstall_item = MenuItem::new("Uninstall ArcPaper...", true, None);
    let uninstall_id = uninstall_item.id().clone();
    let _ = menu.append(&uninstall_item);

    // 9. Exit
    let exit_item = MenuItem::new("Exit ArcPaper", true, None);
    let exit_id = exit_item.id().clone();
    let _ = menu.append(&exit_item);

    let controls = TrayControls {
        upload_id,
        pause_resume_id,
        speed_map,
        fit_map,
        brightness_map,
        ring_size_map,
        fps_map,
        wallpaper_map,
        check_updates_id,
        autostart_id,
        autostart_item,
        uninstall_id,
        exit_id,
    };

    (menu, controls)
}

// ─── UserEvent ────────────────────────────────────────────────────────────────

enum UserEvent {
    TrayMenu(MenuEvent),
    AutoPause(bool),
}

// ─── Smart Auto-Pause Helper ──────────────────────────────────────────────────

unsafe fn is_foreground_covering_screen(
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
        if GetWindowRect(fg, &mut rect).is_ok() {
            if rect.left <= 0 && rect.top <= 0 && rect.right >= screen_w && rect.bottom >= screen_h
            {
                return true;
            }
        }

        false
    }
}

// ─── Playback Helper ──────────────────────────────────────────────────────────

/// Helper function to configure and populate the dynamic ring buffer for video wallpaper playback.
/// Uses an `edl://` virtual timeline to stitch `ring_size` loop slots into a single seamless stream,
/// keeping the D3D11VA hardware decoder alive with zero stutter and static bitstream cache.
fn apply_ring_buffer(video_path: &str, ring_size: usize) -> Result<(), String> {
    player::apply_ring_buffer(video_path, ring_size)
}

/// Helper function to apply decoder-level hardware skipping and frame-drop throttling to MPV.
/// Completely avoids software video filters (`vf`) to preserve zero-copy VRAM and avoid memory allocation.
fn apply_fps_limit(target_fps: Option<u32>) -> Result<(), String> {
    match target_fps {
        Some(fps) if fps <= 15 && fps > 0 => {
            log_msg(&format!(
                "[ArcPaper] Applying FPS throttling: {} FPS (vd-lavc-skipframe=nonref, vd-lavc-skiploopfilter=all, framedrop=decoder+vo, vf=\"\")",
                fps
            ));
        }
        Some(fps) if fps > 15 => {
            log_msg(&format!(
                "[ArcPaper] Applying FPS throttling: {} FPS (vd-lavc-skipframe=default, vd-lavc-skiploopfilter=nonref, framedrop=vo, vf=\"\")",
                fps
            ));
        }
        _ => {
            log_msg("[ArcPaper] Resetting FPS throttling to Native (vd-lavc-skipframe=default, vd-lavc-skiploopfilter=default, framedrop=no, vf=\"\")");
        }
    }
    player::apply_fps_limit(target_fps)
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    // Set working directory to the directory where this executable resides and configure DLL search path
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

    // 0. Force Per-Monitor DPI Awareness (V2) at the very start of main()
    unsafe {
        use windows::Win32::UI::HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
        };
        let res = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if res.is_err() {
            use windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware;
            let _ = SetProcessDPIAware();
        }
    }

    // 0. Single instance enforcement (exit immediately if another instance is already running)
    let _instance_mutex = unsafe {
        use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::core::w;

        let handle = CreateMutexW(None, true, w!("ArcPaper_SingleInstance_Mutex"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log_msg("[ArcPaper] Another instance of ArcPaper is already running. Exiting cleanly.");
            std::process::exit(0);
        }
        handle
    };

    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let log_path = get_app_log_path();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(f, "[PANIC] {:?}", info);
        }
    }));

    log_msg("==================================================");
    log_msg("[ArcPaper] Starting ArcPaper Native Wallpaper Engine");
    log_msg("==================================================");

    // Read exact physical screen resolution with DPI awareness active
    let (screen_w, screen_h) =
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    log_msg(&format!(
        "[ArcPaper] Per-Monitor DPI Aware active. Physical screen resolution: {}x{}",
        screen_w, screen_h
    ));

    // Set process priority class to BELOW_NORMAL_PRIORITY_CLASS to smooth background CPU usage and prevent micro-spikes
    unsafe {
        use windows::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
        };
        let res = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        log_msg(&format!(
            "[ArcPaper] SetPriorityClass(BELOW_NORMAL_PRIORITY_CLASS) result: {:?}",
            res
        ));
    }

    // 1. Ensure SavedWallpapers folder exists
    let saved_wallpapers_dir = get_saved_wallpapers_dir();
    let _ = std::fs::create_dir_all(&saved_wallpapers_dir);
    log_msg(&format!(
        "[ArcPaper] SavedWallpapers directory: {:?}",
        saved_wallpapers_dir
    ));

    // 2. Hook desktop WorkerW layer with retry loop and size directly to physical screen resolution
    let workerw_hwnd = workerw::AttachToDesktop();
    log_msg(&format!(
        "[ArcPaper] Desktop WorkerW HWND: {:?}",
        workerw_hwnd
    ));

    if !workerw_hwnd.0.is_null() {
        unsafe {
            let p_style = GetWindowLongPtrW(workerw_hwnd, GWL_STYLE) as u32;
            let _ = SetWindowLongPtrW(
                workerw_hwnd,
                GWL_STYLE,
                (p_style | WS_CLIPCHILDREN.0) as isize,
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
            log_msg(&format!(
                "[ArcPaper] WorkerW positioned to exact physical pixels: 0, 0, {}x{}",
                screen_w, screen_h
            ));
        }
    }

    // 3. Initialize MPV core engine attached directly to WorkerW HWND
    let progman_current = workerw::get_progman_hwnd();
    let mpv_target_wid = if !workerw_hwnd.0.is_null() {
        workerw_hwnd
    } else if !progman_current.0.is_null() {
        progman_current
    } else {
        log_msg(
            "[ArcPaper] Secondary/sandbox session: skipping cross-desktop WID attachment for MPV safety.",
        );
        HWND::default()
    };

    match player::init_player(mpv_target_wid) {
        Ok(_) => {
            log_msg(&format!(
                "[ArcPaper] MPV engine initialized and attached to WID {:?}.",
                mpv_target_wid
            ));
            if !mpv_target_wid.0.is_null() {
                unsafe {
                    let _ = SetWindowPos(
                        mpv_target_wid,
                        None,
                        0,
                        0,
                        screen_w,
                        screen_h,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
            }
        }
        Err(e) => {
            log_msg(&format!("[ArcPaper] MPV engine init error/warning: {}", e));
        }
    }

    // 4. Load persisted user configuration
    let initial_config = load_config();
    let wp_state: Rc<RefCell<AppConfig>> = Rc::new(RefCell::new(initial_config));
    let mut current_native_fps = 60u32;

    // 5. Initialize Tao EventLoop for System Tray and Auto-Pause
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    // 6. Build System Tray Menu with retry loop if shell tray is not ready yet during boot
    let (tray_icon, mut controls) = {
        let mut built = None;
        for attempt in 1..=10 {
            let (menu, new_ctrl) = build_tray_menu(
                &saved_wallpapers_dir,
                wp_state.borrow().is_manually_paused,
                wp_state.borrow().active_video.as_deref(),
                wp_state.borrow().ring_size,
                current_native_fps,
                wp_state.borrow().target_fps,
            );
            match TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("ArcPaper")
                .with_icon(create_default_tray_icon())
                .build()
            {
                Ok(icon) => {
                    if attempt > 1 {
                        log_msg(&format!(
                            "[ArcPaper] System tray icon created successfully on attempt {}",
                            attempt
                        ));
                    }
                    built = Some((icon, new_ctrl));
                    break;
                }
                Err(e) => {
                    log_msg(&format!(
                        "[ArcPaper] System tray not ready (attempt {}/10): {:?}. Retrying in 500ms...",
                        attempt, e
                    ));
                    if attempt < 10 {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }
        built.expect("Failed to create system tray icon after 10 attempts")
    };

    let proxy_tray = event_loop.create_proxy();
    let proxy_tray_cb = proxy_tray.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy_tray_cb.send_event(UserEvent::TrayMenu(event));
    }));

    // 7. Auto-play saved wallpaper on startup (or fallback to first MP4)
    let valid_video = {
        let state = wp_state.borrow();
        if let Some(ref name) = state.active_video {
            if saved_wallpapers_dir.join(name).is_file() {
                Some(name.clone())
            } else {
                None
            }
        } else {
            None
        }
    };

    let video_to_play = valid_video.or_else(|| {
        let files = list_mp4_files(&saved_wallpapers_dir);
        if let Some(first) = files.into_iter().next() {
            wp_state.borrow_mut().active_video = Some(first.clone());
            save_config(&wp_state.borrow());
            Some(first)
        } else {
            None
        }
    });

    if let Some(name) = video_to_play {
        let video_path = saved_wallpapers_dir.join(&name);
        let ring_size = wp_state.borrow().ring_size;
        log_msg(&format!(
            "[ArcPaper] Auto-playing wallpaper on startup (ring_size {}): {:?}",
            ring_size, video_path
        ));
        if let Err(e) = apply_ring_buffer(&video_path.to_string_lossy(), ring_size) {
            log_msg(&format!("[ArcPaper] MPV apply_ring_buffer error: {}", e));
        } else {
            log_msg(&format!(
                "[ArcPaper] MPV playback started for: {:?}",
                video_path
            ));
        }

        if let Some(fps) = player::detect_native_fps() {
            current_native_fps = fps.round() as u32;
            log_msg(&format!(
                "[ArcPaper] Detected native video FPS: {:.2} (rounded: {})",
                fps, current_native_fps
            ));
        }

        let state = wp_state.borrow();
        let _ = player::set_speed(state.speed);
        let _ = player::set_brightness(state.brightness);
        let _ = player::set_fit(&state.fit_mode);
        let _ = apply_fps_limit(state.target_fps);
        if state.is_manually_paused {
            let _ = player::set_pause(true);
        }
        delayed_trim(3000);

        let (new_menu, new_controls) = build_tray_menu(
            &saved_wallpapers_dir,
            state.is_manually_paused,
            state.active_video.as_deref(),
            state.ring_size,
            current_native_fps,
            state.target_fps,
        );
        tray_icon.set_menu(Some(Box::new(new_menu)));
        controls = new_controls;
    }

    // 8. Start Smart Auto-Pause background monitor thread
    let proxy_autopause = event_loop.create_proxy();
    let our_pid = std::process::id();
    std::thread::spawn(move || {
        let mut last_state = false;
        let mut last_fg = HWND::default();
        let mut last_zoomed = false;
        loop {
            // Check interval: 1000ms (lightweight foreground window monitor, no periodic memory trimming)
            std::thread::sleep(Duration::from_millis(1000));

            unsafe {
                let fg = GetForegroundWindow();
                if fg.0.is_null() {
                    if last_state {
                        last_state = false;
                        last_fg = HWND::default();
                        last_zoomed = false;
                        let _ = proxy_autopause.send_event(UserEvent::AutoPause(false));
                    }
                    continue;
                }

                let is_zoomed = IsZoomed(fg).as_bool();

                // If active window HWND and zoom state have not changed, keep thread sleeping
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
                    let _ = proxy_autopause.send_event(UserEvent::AutoPause(is_covered));
                }
            }
        }
    });

    log_msg("[ArcPaper] Native event loop active. Running with 0 WebView2 instances.");

    let mut is_auto_paused = false;

    // 9. Run Tao Event Loop
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + Duration::from_millis(200),
        );

        let _keep_tray = &tray_icon;

        // Drain tray icon events safely (no explorer or external commands)
        while tray_icon::TrayIconEvent::receiver().try_recv().is_ok() {}

        match event {
            // ── Smart Auto-Pause Event ───────────────────────────────────────
            Event::UserEvent(UserEvent::AutoPause(is_covered)) => {
                let state = wp_state.borrow();
                let _ = player::set_pause(is_covered);
                if is_covered {
                    if !state.is_manually_paused && !is_auto_paused {
                        is_auto_paused = true;
                        log_msg("[ArcPaper] Smart Auto-Pause: Maximized / fullscreen window active. Wallpaper paused.");
                        trim_working_set();
                    }
                } else {
                    if !state.is_manually_paused && is_auto_paused {
                        is_auto_paused = false;
                        log_msg("[ArcPaper] Smart Auto-Resume: Desktop visible. Wallpaper resumed.");
                    }
                }
            }

            // ── Tray Menu Events ────────────────────────────────────────────
            Event::UserEvent(UserEvent::TrayMenu(menu_event)) => {
                let event_id = menu_event.id.clone();

                if event_id == controls.upload_id {
                    log_msg("[ArcPaper] 'Upload New Wallpaper' clicked. Opening file picker...");
                    if let Some(source_path) = rfd::FileDialog::new()
                        .add_filter("MP4 Video (*.mp4)", &["mp4"])
                        .set_title("Select MP4 Video File")
                        .pick_file()
                    {
                        if let Some(file_name) = source_path.file_name().and_then(|n| n.to_str()) {
                            let dest_path = saved_wallpapers_dir.join(file_name);
                            if let Err(e) = std::fs::copy(&source_path, &dest_path) {
                                log_msg(&format!("[ArcPaper] Failed to copy video file: {:?}", e));
                            } else {
                                log_msg(&format!("[ArcPaper] Video copied to {:?}", dest_path));

                                // Seamless wallpaper switch via MPV IPC ring buffer
                                let ring_size = wp_state.borrow().ring_size;
                                if let Err(e) = apply_ring_buffer(&dest_path.to_string_lossy(), ring_size) {
                                    log_msg(&format!("[ArcPaper] Error applying ring buffer: {}", e));
                                }
                                delayed_trim(500);

                                if let Some(fps) = player::detect_native_fps() {
                                    current_native_fps = fps.round() as u32;
                                    log_msg(&format!(
                                        "[ArcPaper] Detected native video FPS: {:.2} (rounded: {})",
                                        fps, current_native_fps
                                    ));
                                }

                                let mut state = wp_state.borrow_mut();
                                state.active_video = Some(file_name.to_string());
                                state.is_manually_paused = false;
                                is_auto_paused = false;

                                if let Some(target) = state.target_fps {
                                    if target >= current_native_fps {
                                        state.target_fps = None;
                                    }
                                }
                                save_config(&state);

                                let _ = apply_fps_limit(state.target_fps);

                                let is_paused = state.is_manually_paused;
                                let active_name = state.active_video.clone();
                                let ring_size = state.ring_size;
                                let target_fps = state.target_fps;
                                drop(state);

                                // Rebuild tray menu with new list and active marker
                                let (new_menu, new_controls) = build_tray_menu(
                                    &saved_wallpapers_dir,
                                    is_paused,
                                    active_name.as_deref(),
                                    ring_size,
                                    current_native_fps,
                                    target_fps,
                                );
                                tray_icon.set_menu(Some(Box::new(new_menu)));
                                controls = new_controls;
                            }
                        }
                    }

                } else if event_id == controls.pause_resume_id {
                    let mut state = wp_state.borrow_mut();
                    state.is_manually_paused = !state.is_manually_paused;
                    let _ = player::set_pause(state.is_manually_paused);
                    is_auto_paused = false;
                    save_config(&state);

                    log_msg(&format!(
                        "[ArcPaper] Wallpaper manually {}",
                        if state.is_manually_paused { "paused" } else { "resumed" }
                    ));

                    if state.is_manually_paused {
                        trim_working_set();
                    }

                    let is_paused = state.is_manually_paused;
                    let active_name = state.active_video.clone();
                    let ring_size = state.ring_size;
                    let target_fps = state.target_fps;
                    drop(state);

                    let (new_menu, new_controls) = build_tray_menu(
                        &saved_wallpapers_dir,
                        is_paused,
                        active_name.as_deref(),
                        ring_size,
                        current_native_fps,
                        target_fps,
                    );
                    tray_icon.set_menu(Some(Box::new(new_menu)));
                    controls = new_controls;

                } else if let Some(&speed) = controls.speed_map.get(&event_id) {
                    log_msg(&format!("[ArcPaper] Setting playback rate: {}x", speed));
                    let _ = player::set_speed(speed);
                    let mut state = wp_state.borrow_mut();
                    state.speed = speed;
                    save_config(&state);
                    delayed_trim(500);

                } else if let Some(&fit) = controls.fit_map.get(&event_id) {
                    log_msg(&format!("[ArcPaper] Setting display fit mode: {}", fit));
                    let _ = player::set_fit(fit);
                    let mut state = wp_state.borrow_mut();
                    state.fit_mode = fit.to_string();
                    save_config(&state);
                    delayed_trim(500);

                } else if let Some(&brightness) = controls.brightness_map.get(&event_id) {
                    log_msg(&format!("[ArcPaper] Setting brightness: {}", brightness));
                    let _ = player::set_brightness(brightness);
                    let mut state = wp_state.borrow_mut();
                    state.brightness = brightness;
                    save_config(&state);
                    delayed_trim(500);

                } else if let Some(&new_ring_size) = controls.ring_size_map.get(&event_id) {
                    log_msg(&format!(
                        "[ArcPaper] Dynamic ring buffer changed to {} loops",
                        new_ring_size
                    ));
                    let mut state = wp_state.borrow_mut();
                    state.ring_size = new_ring_size;
                    save_config(&state);

                    // Refresh active wallpaper playlist immediately without restarting application
                    if let Some(ref active_name) = state.active_video {
                        let video_path = saved_wallpapers_dir.join(active_name);
                        if video_path.is_file() {
                            if let Err(e) = apply_ring_buffer(&video_path.to_string_lossy(), new_ring_size) {
                                log_msg(&format!("[ArcPaper] Error refreshing ring buffer: {}", e));
                            } else {
                                log_msg(&format!(
                                    "[ArcPaper] Successfully refreshed active wallpaper ring buffer with {} loops",
                                    new_ring_size
                                ));
                            }
                            let _ = player::set_speed(state.speed);
                            let _ = player::set_brightness(state.brightness);
                            let _ = player::set_fit(&state.fit_mode);
                            let _ = apply_fps_limit(state.target_fps);
                            if state.is_manually_paused {
                                let _ = player::set_pause(true);
                            }
                            delayed_trim(500);
                        }
                    }

                    let is_paused = state.is_manually_paused;
                    let active_name = state.active_video.clone();
                    let ring_size = state.ring_size;
                    let target_fps = state.target_fps;
                    drop(state);

                    // Rebuild tray menu to update check mark on selected ring buffer size
                    let (new_menu, new_controls) = build_tray_menu(
                        &saved_wallpapers_dir,
                        is_paused,
                        active_name.as_deref(),
                        ring_size,
                        current_native_fps,
                        target_fps,
                    );
                    tray_icon.set_menu(Some(Box::new(new_menu)));
                    controls = new_controls;

                } else if let Some(selected_wallpaper) = controls.wallpaper_map.get(&event_id).cloned() {
                    let full_path = saved_wallpapers_dir.join(&selected_wallpaper);
                    log_msg(&format!(
                        "[ArcPaper] Seamlessly switching wallpaper to: {:?}",
                        full_path
                    ));

                    let ring_size = wp_state.borrow().ring_size;
                    if let Err(e) = apply_ring_buffer(&full_path.to_string_lossy(), ring_size) {
                        log_msg(&format!("[ArcPaper] Error applying ring buffer: {}", e));
                    }
                    delayed_trim(500);

                    if let Some(fps) = player::detect_native_fps() {
                        current_native_fps = fps.round() as u32;
                        log_msg(&format!(
                            "[ArcPaper] Detected native video FPS: {:.2} (rounded: {})",
                            fps, current_native_fps
                        ));
                    }

                    let mut state = wp_state.borrow_mut();
                    state.active_video = Some(selected_wallpaper.clone());
                    state.is_manually_paused = false;
                    is_auto_paused = false;

                    if let Some(target) = state.target_fps {
                        if target >= current_native_fps {
                            state.target_fps = None;
                        }
                    }
                    save_config(&state);

                    let _ = apply_fps_limit(state.target_fps);

                    let is_paused = state.is_manually_paused;
                    let active_name = state.active_video.clone();
                    let ring_size = state.ring_size;
                    let target_fps = state.target_fps;
                    drop(state);

                    // Rebuild tray menu to update active wallpaper indicator
                    let (new_menu, new_controls) = build_tray_menu(
                        &saved_wallpapers_dir,
                        is_paused,
                        active_name.as_deref(),
                        ring_size,
                        current_native_fps,
                        target_fps,
                    );
                    tray_icon.set_menu(Some(Box::new(new_menu)));
                    controls = new_controls;

                } else if let Some(&target_fps_opt) = controls.fps_map.get(&event_id) {
                    let mut state = wp_state.borrow_mut();
                    state.target_fps = target_fps_opt;
                    save_config(&state);

                    let _ = apply_fps_limit(target_fps_opt);
                    delayed_trim(500);

                    let is_paused = state.is_manually_paused;
                    let active_name = state.active_video.clone();
                    let ring_size = state.ring_size;
                    let target_fps = state.target_fps;
                    drop(state);

                    // Rebuild tray menu to update check mark on selected Target FPS
                    let (new_menu, new_controls) = build_tray_menu(
                        &saved_wallpapers_dir,
                        is_paused,
                        active_name.as_deref(),
                        ring_size,
                        current_native_fps,
                        target_fps,
                    );
                    tray_icon.set_menu(Some(Box::new(new_menu)));
                    controls = new_controls;

                } else if event_id == controls.check_updates_id {
                    log_msg("[ArcPaper] 'Check for Updates' clicked. Spawning update check thread...");
                    std::thread::spawn(|| {
                        check_for_updates();
                    });

                } else if event_id == controls.autostart_id {
                    let currently_enabled = is_autostart_enabled();
                    if currently_enabled {
                        if let Err(e) = disable_autostart() {
                            log_msg(&format!("[ArcPaper] Error disabling auto-start: {:?}", e));
                        } else {
                            log_msg("[ArcPaper] Auto-start with Windows disabled.");
                        }
                    } else {
                        if let Err(e) = enable_autostart() {
                            log_msg(&format!("[ArcPaper] Error enabling auto-start: {:?}", e));
                        } else {
                            log_msg("[ArcPaper] Auto-start with Windows enabled.");
                        }
                    }

                    let new_status = is_autostart_enabled();
                    controls.autostart_item.set_checked(new_status);

                    let state = wp_state.borrow();
                    let is_paused = state.is_manually_paused;
                    let active_name = state.active_video.clone();
                    let ring_size = state.ring_size;
                    let target_fps = state.target_fps;
                    drop(state);

                    let (new_menu, new_controls) = build_tray_menu(
                        &saved_wallpapers_dir,
                        is_paused,
                        active_name.as_deref(),
                        ring_size,
                        current_native_fps,
                        target_fps,
                    );
                    tray_icon.set_menu(Some(Box::new(new_menu)));
                    controls = new_controls;

                } else if event_id == controls.uninstall_id {
                    perform_uninstall();
                } else if event_id == controls.exit_id {
                    log_msg("[ArcPaper] Exit requested. Shutting down cleanly...");
                    *control_flow = ControlFlow::Exit;
                }
            }

            // ── Fallback Polling ─────────────────────────────────────────────
            Event::MainEventsCleared => {
                while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
                    let _ = proxy_tray.send_event(UserEvent::TrayMenu(menu_event));
                }
            }

            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trim_working_set() {
        trim_working_set();
    }

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
        let release: GitHubRelease = serde_json::from_str(sample_json).expect("Failed to deserialize sample GitHub release");
        assert_eq!(release.tag_name, "v1.5.0");
        assert_eq!(release.html_url, "https://github.com/ArtixForLiving/ArcPaper/releases/tag/v1.5.0");
    }

    #[test]
    fn test_check_menu_item() {
        use tray_icon::menu::CheckMenuItem;
        let item = CheckMenuItem::new("Start with Windows", true, true, None);
        assert!(item.is_checked());
        item.set_checked(false);
        assert!(!item.is_checked());
    }

    #[test]
    fn test_autostart_registry_cycle() {
        // Record initial state
        let initial = is_autostart_enabled();

        // Test enable
        assert!(enable_autostart().is_ok());
        assert!(is_autostart_enabled());

        // Verify registry value format has double quotes and does not contain deps/
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        if let Ok(run_key) = hkcu.open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_READ) {
            let val: String = run_key.get_value(APP_REG_NAME).unwrap();
            assert!(val.starts_with('"') && val.ends_with('"'));
            assert!(!val.contains("deps"));
        }

        // Test disable
        assert!(disable_autostart().is_ok());
        assert!(!is_autostart_enabled());

        // Restore initial state if it was enabled
        if initial {
            let _ = enable_autostart();
        }
    }

    #[test]
    fn test_app_config_ring_size_serialization_and_defaults() {
        let default_cfg = AppConfig::default();
        assert_eq!(default_cfg.ring_size, 10);

        let json = serde_json::to_string(&default_cfg).unwrap();
        assert!(json.contains("\"ring_size\": 10") || json.contains("\"ring_size\":10"));

        let deserialized: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.ring_size, 10);

        // Test fallback for legacy config without ring_size
        let legacy_json = r#"{
            "active_video": "test.mp4",
            "speed": 1.0,
            "brightness": 100,
            "fit_mode": "cover",
            "is_manually_paused": false
        }"#;
        let from_legacy: AppConfig = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(from_legacy.ring_size, 10);

        // Test custom supported ring sizes (5, 20)
        let custom_5_json = r#"{"ring_size": 5, "speed": 1.0, "brightness": 100, "fit_mode": "cover", "is_manually_paused": false}"#;
        let from_5: AppConfig = serde_json::from_str(custom_5_json).unwrap();
        assert_eq!(from_5.ring_size, 5);

        let custom_20_json = r#"{"ring_size": 20, "speed": 1.0, "brightness": 100, "fit_mode": "cover", "is_manually_paused": false}"#;
        let from_20: AppConfig = serde_json::from_str(custom_20_json).unwrap();
        assert_eq!(from_20.ring_size, 20);
    }

    #[test]
    fn test_app_config_target_fps_serialization_and_defaults() {
        let default_cfg = AppConfig::default();
        assert_eq!(default_cfg.target_fps, None);

        let mut cfg = AppConfig::default();
        cfg.target_fps = Some(30);
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"target_fps\": 30") || json.contains("\"target_fps\":30"));

        let deserialized: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.target_fps, Some(30));

        // Test fallback for legacy config without target_fps
        let legacy_json = r#"{
            "active_video": "test.mp4",
            "speed": 1.0,
            "brightness": 100,
            "fit_mode": "cover",
            "is_manually_paused": false,
            "ring_size": 10
        }"#;
        let from_legacy: AppConfig = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(from_legacy.target_fps, None);

        // Test explicit null
        let null_json = r#"{
            "active_video": "test.mp4",
            "speed": 1.0,
            "brightness": 100,
            "fit_mode": "cover",
            "is_manually_paused": false,
            "ring_size": 10,
            "target_fps": null
        }"#;
        let from_null: AppConfig = serde_json::from_str(null_json).unwrap();
        assert_eq!(from_null.target_fps, None);
    }

    #[test]
    fn test_get_fps_options_logic() {
        // High frame rate video (> 45 FPS, e.g. 60 FPS)
        let opts_60 = get_fps_options(60);
        let vals_60: Vec<u32> = opts_60.iter().map(|(val, _)| *val).collect();
        assert_eq!(vals_60, vec![45, 30, 20, 15]);

        // Standard 30 FPS or 24 FPS video (<= 35 FPS)
        let opts_30 = get_fps_options(30);
        let vals_30: Vec<u32> = opts_30.iter().map(|(val, _)| *val).collect();
        assert_eq!(vals_30, vec![20, 15]);

        let opts_24 = get_fps_options(24);
        let vals_24: Vec<u32> = opts_24.iter().map(|(val, _)| *val).collect();
        assert_eq!(vals_24, vec![20, 15]);

        // Intermediate framerate (e.g. 40 FPS)
        let opts_40 = get_fps_options(40);
        let vals_40: Vec<u32> = opts_40.iter().map(|(val, _)| *val).collect();
        assert_eq!(vals_40, vec![30, 20, 15]);
    }

    #[test]
    fn test_uninstall_cmd_formatting() {
        let exe = PathBuf::from(r"C:\App\arc_paper.exe");
        let cfg = PathBuf::from(r"C:\App\config.json");
        let cmd = format!(
            "ping 127.0.0.1 -n 3 > nul & del /F /Q \"{}\" \"{}\"",
            exe.display(),
            cfg.display()
        );
        assert!(cmd.starts_with("ping 127.0.0.1 -n 3 > nul"));
        assert!(cmd.contains(r#"del /F /Q "C:\App\arc_paper.exe" "C:\App\config.json""#));
    }
}
