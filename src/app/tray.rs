//! System tray icon rendering and hierarchical menu builder.

use super::autostart::is_autostart_enabled;
use super::config::AppConfig;
use super::log::{LogLevel, log_msg, log_with_level};
use super::paths::list_mp4_files;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct TrayControls {
    pub upload_id: MenuId,
    pub pause_resume_id: MenuId,
    pub speed_map: HashMap<MenuId, f32>,
    pub fit_map: HashMap<MenuId, &'static str>,
    pub brightness_map: HashMap<MenuId, u32>,
    pub fps_map: HashMap<MenuId, Option<u32>>,
    pub wallpaper_map: HashMap<MenuId, String>,
    pub check_updates_id: MenuId,
    pub autostart_id: MenuId,
    pub autostart_item: CheckMenuItem,
    pub uninstall_id: MenuId,
    pub exit_id: MenuId,
}

/// Creates a crisp 32x32 RGBA icon for the system tray.
pub fn create_default_tray_icon() -> Icon {
    let width = 32u32;
    let height = 32u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);

    for y in 0..height {
        for x in 0..width {
            let is_border = x == 0 || x == width - 1 || y == 0 || y == height - 1;
            let is_inner_accent = ((8..=23).contains(&x) && (8..=23).contains(&y))
                && (x == 8 || x == 23 || y == 8 || y == 23);

            if is_border {
                rgba.extend_from_slice(&[59, 130, 246, 255]);
            } else if is_inner_accent {
                rgba.extend_from_slice(&[100, 255, 218, 255]);
            } else {
                rgba.extend_from_slice(&[10, 25, 47, 255]);
            }
        }
    }

    Icon::from_rgba(rgba, width, height).expect("Failed to create system tray icon")
}

/// Returns dynamic target FPS options based on native video frame rate.
pub fn get_fps_options(native_fps: u32) -> &'static [(u32, &'static str)] {
    if native_fps > 45 {
        &[
            (45, "45 FPS"),
            (30, "30 FPS"),
            (20, "20 FPS"),
            (15, "15 FPS (Power Saver)"),
        ]
    } else if native_fps > 35 {
        &[(30, "30 FPS"), (20, "20 FPS"), (15, "15 FPS (Power Saver)")]
    } else {
        &[(20, "20 FPS"), (15, "15 FPS (Power Saver)")]
    }
}

pub fn build_tray_menu(
    saved_wallpapers_dir: &Path,
    is_paused: bool,
    active_wp: Option<&str>,
    native_fps: u32,
    target_fps: Option<u32>,
) -> (Menu, TrayControls) {
    let menu = Menu::new();

    let upload_item = MenuItem::new("Upload New Wallpaper...", true, None);
    let upload_id = upload_item.id().clone();
    let _ = menu.append(&upload_item);

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

    let pause_text = if is_paused {
        "Resume Wallpaper"
    } else {
        "Pause Wallpaper"
    };
    let pause_resume_item = MenuItem::new(pause_text, true, None);
    let pause_resume_id = pause_resume_item.id().clone();
    let _ = menu.append(&pause_resume_item);

    let _ = menu.append(&PredefinedMenuItem::separator());

    let visuals_submenu = Submenu::new("Display & Visuals", true);

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

    let performance_submenu = Submenu::new("Performance", true);

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

    let fps_submenu = Submenu::new("Target FPS", true);
    let mut fps_map = HashMap::new();

    let is_native = target_fps.unwrap_or(0) == 0;
    let native_item = CheckMenuItem::new("Native (Original)", true, is_native, None);
    fps_map.insert(native_item.id().clone(), None);
    let _ = fps_submenu.append(&native_item);

    for &(fps_val, label) in get_fps_options(native_fps) {
        let is_checked = target_fps == Some(fps_val);
        let item = CheckMenuItem::new(label, true, is_checked, None);
        fps_map.insert(item.id().clone(), Some(fps_val));
        let _ = fps_submenu.append(&item);
    }
    let _ = performance_submenu.append(&fps_submenu);

    let _ = menu.append(&performance_submenu);

    let _ = menu.append(&PredefinedMenuItem::separator());

    let check_updates_item = MenuItem::new("Check for Updates...", true, None);
    let check_updates_id = check_updates_item.id().clone();
    let _ = menu.append(&check_updates_item);

    let autostart_enabled = is_autostart_enabled();
    let autostart_item = CheckMenuItem::new("Start with Windows", true, autostart_enabled, None);
    let autostart_id = autostart_item.id().clone();
    let _ = menu.append(&autostart_item);

    let uninstall_item = MenuItem::new("Uninstall ArcPaper...", true, None);
    let uninstall_id = uninstall_item.id().clone();
    let _ = menu.append(&uninstall_item);

    let exit_item = MenuItem::new("Exit ArcPaper", true, None);
    let exit_id = exit_item.id().clone();
    let _ = menu.append(&exit_item);

    let controls = TrayControls {
        upload_id,
        pause_resume_id,
        speed_map,
        fit_map,
        brightness_map,
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

/// Builds the tray icon with up to 10 retries (500ms apart) while Windows shell tray initializes.
pub fn build_initial_tray_icon(
    saved_wallpapers_dir: &Path,
    state: &AppConfig,
    native_fps: u32,
) -> (TrayIcon, TrayControls) {
    let mut built = None;
    for attempt in 1..=10 {
        let (menu, new_ctrl) = build_tray_menu(
            saved_wallpapers_dir,
            state.is_manually_paused,
            state.active_video.as_deref(),
            native_fps,
            state.target_fps,
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
                log_with_level(
                    LogLevel::Warn,
                    &format!(
                        "[ArcPaper] System tray not ready (attempt {}/10): {:?}. Retrying in 500ms...",
                        attempt, e
                    ),
                );
                if attempt < 10 {
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }
    built.expect("Failed to create system tray icon after 10 attempts")
}

/// Rebuilds and attaches an updated tray menu reflecting the current player state.
pub fn rebuild_tray(
    tray_icon: &TrayIcon,
    saved_wallpapers_dir: &Path,
    state: &AppConfig,
    native_fps: u32,
) -> TrayControls {
    let (new_menu, new_controls) = build_tray_menu(
        saved_wallpapers_dir,
        state.is_manually_paused,
        state.active_video.as_deref(),
        native_fps,
        state.target_fps,
    );
    tray_icon.set_menu(Some(Box::new(new_menu)));
    new_controls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_menu_item() {
        let item = CheckMenuItem::new("Start with Windows", true, true, None);
        assert!(item.is_checked());
        item.set_checked(false);
        assert!(!item.is_checked());
    }

    #[test]
    fn test_get_fps_options_logic() {
        let opts_60 = get_fps_options(60);
        let vals_60: Vec<u32> = opts_60.iter().map(|(val, _)| *val).collect();
        assert_eq!(vals_60, vec![45, 30, 20, 15]);

        let opts_30 = get_fps_options(30);
        let vals_30: Vec<u32> = opts_30.iter().map(|(val, _)| *val).collect();
        assert_eq!(vals_30, vec![20, 15]);

        let opts_24 = get_fps_options(24);
        let vals_24: Vec<u32> = opts_24.iter().map(|(val, _)| *val).collect();
        assert_eq!(vals_24, vec![20, 15]);

        let opts_40 = get_fps_options(40);
        let vals_40: Vec<u32> = opts_40.iter().map(|(val, _)| *val).collect();
        assert_eq!(vals_40, vec![30, 20, 15]);
    }
}
