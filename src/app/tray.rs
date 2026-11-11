//! System tray icon rendering and hierarchical menu builder.

use super::autostart::is_autostart_enabled;
use super::config::{AppConfig, DisplayMode, ResolvedMonitorConfig};
use super::log::{LogLevel, log_msg, log_with_level};
use super::paths::list_mp4_files;
use crate::display::{MonitorId, MonitorInfo};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction {
    UploadWallpaper(Option<MonitorId>),
    SelectWallpaper {
        monitor_id: Option<MonitorId>,
        file: String,
    },
    SetDisplayMode(DisplayMode),
    TogglePause(Option<MonitorId>),
    SetFit {
        monitor_id: Option<MonitorId>,
        fit: &'static str,
    },
    SetBrightness {
        monitor_id: Option<MonitorId>,
        brightness: u32,
    },
    SetSpeed {
        monitor_id: Option<MonitorId>,
        speed: f32,
    },
    SetTargetFps {
        monitor_id: Option<MonitorId>,
        target_fps: Option<u32>,
    },
    CheckUpdates,
    ToggleAutostart,
    Uninstall,
    Exit,
}

pub struct TrayControls {
    pub actions: HashMap<MenuId, TrayAction>,
    pub autostart_item: CheckMenuItem,
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

enum MenuParent<'a> {
    Menu(&'a Menu),
    Submenu(&'a Submenu),
}

impl MenuParent<'_> {
    fn append(&self, item: &impl tray_icon::menu::IsMenuItem) {
        match self {
            MenuParent::Menu(m) => {
                let _ = m.append(item);
            }
            MenuParent::Submenu(s) => {
                let _ = s.append(item);
            }
        }
    }
}

struct TargetControlContext<'a> {
    saved_wallpapers_dir: &'a Path,
    config: &'a AppConfig,
    native_fps_by_monitor: &'a BTreeMap<MonitorId, u32>,
    global_native_fps: u32,
    is_per_monitor_multi: bool,
}

fn append_target_controls(
    parent: &MenuParent<'_>,
    target: Option<&MonitorId>,
    ctx: &TargetControlContext<'_>,
    actions: &mut HashMap<MenuId, TrayAction>,
) {
    let resolved = match target {
        Some(mid) => ctx.config.resolve_for_monitor(mid),
        None => ResolvedMonitorConfig {
            active_video: ctx.config.active_video.clone(),
            speed: ctx.config.speed,
            brightness: ctx.config.brightness,
            fit_mode: ctx.config.fit_mode.clone(),
            is_manually_paused: ctx.config.is_manually_paused,
            target_fps: ctx.config.target_fps,
        },
    };

    let native_fps = match target {
        Some(mid) => ctx
            .native_fps_by_monitor
            .get(mid)
            .copied()
            .unwrap_or(ctx.global_native_fps),
        None => ctx.global_native_fps,
    };

    // 1. Upload Wallpaper
    let upload_item = MenuItem::new("Upload New Wallpaper...", true, None);
    actions.insert(
        upload_item.id().clone(),
        TrayAction::UploadWallpaper(target.cloned()),
    );
    parent.append(&upload_item);

    // 2. Select Active Wallpaper Submenu
    let apply_submenu = Submenu::new("Select Active Wallpaper", true);
    let mp4_files = list_mp4_files(ctx.saved_wallpapers_dir);
    if mp4_files.is_empty() {
        let empty_item = MenuItem::new("No saved wallpapers (.mp4)", false, None);
        let _ = apply_submenu.append(&empty_item);
    } else {
        for filename in mp4_files {
            let label = if resolved.active_video.as_deref() == Some(&filename) {
                format!("* {}", filename)
            } else {
                filename.clone()
            };
            let item = MenuItem::new(&label, true, None);
            actions.insert(
                item.id().clone(),
                TrayAction::SelectWallpaper {
                    monitor_id: target.cloned(),
                    file: filename,
                },
            );
            let _ = apply_submenu.append(&item);
        }
    }
    parent.append(&apply_submenu);

    // 3. Pause / Resume Wallpaper
    let pause_label = if target.is_none() && ctx.is_per_monitor_multi {
        if resolved.is_manually_paused {
            "Resume All Wallpapers"
        } else {
            "Pause All Wallpapers"
        }
    } else if resolved.is_manually_paused {
        "Resume Wallpaper"
    } else {
        "Pause Wallpaper"
    };
    let pause_item = MenuItem::new(pause_label, true, None);
    actions.insert(
        pause_item.id().clone(),
        TrayAction::TogglePause(target.cloned()),
    );
    parent.append(&pause_item);

    // 4. Separator
    parent.append(&PredefinedMenuItem::separator());

    // 5. Display & Visuals Submenu
    let visuals_submenu = Submenu::new("Display & Visuals", true);

    let brightness_submenu = Submenu::new("Brightness", true);
    let bright_100 = MenuItem::new("100% (Normal)", true, None);
    let bright_75 = MenuItem::new("75% (Dim)", true, None);
    let bright_50 = MenuItem::new("50% (Dark)", true, None);
    actions.insert(
        bright_100.id().clone(),
        TrayAction::SetBrightness {
            monitor_id: target.cloned(),
            brightness: 100,
        },
    );
    actions.insert(
        bright_75.id().clone(),
        TrayAction::SetBrightness {
            monitor_id: target.cloned(),
            brightness: 75,
        },
    );
    actions.insert(
        bright_50.id().clone(),
        TrayAction::SetBrightness {
            monitor_id: target.cloned(),
            brightness: 50,
        },
    );
    let _ = brightness_submenu.append(&bright_100);
    let _ = brightness_submenu.append(&bright_75);
    let _ = brightness_submenu.append(&bright_50);
    let _ = visuals_submenu.append(&brightness_submenu);

    let fit_submenu = Submenu::new("Screen Fit", true);
    let fit_cover = MenuItem::new("Cover (Fill & Crop)", true, None);
    let fit_fill = MenuItem::new("Fill (Stretch)", true, None);
    let fit_contain = MenuItem::new("Contain (Original)", true, None);
    actions.insert(
        fit_cover.id().clone(),
        TrayAction::SetFit {
            monitor_id: target.cloned(),
            fit: "cover",
        },
    );
    actions.insert(
        fit_fill.id().clone(),
        TrayAction::SetFit {
            monitor_id: target.cloned(),
            fit: "fill",
        },
    );
    actions.insert(
        fit_contain.id().clone(),
        TrayAction::SetFit {
            monitor_id: target.cloned(),
            fit: "contain",
        },
    );
    let _ = fit_submenu.append(&fit_cover);
    let _ = fit_submenu.append(&fit_fill);
    let _ = fit_submenu.append(&fit_contain);
    let _ = visuals_submenu.append(&fit_submenu);

    parent.append(&visuals_submenu);

    // 6. Performance Submenu
    let performance_submenu = Submenu::new("Performance", true);

    let speed_submenu = Submenu::new("Playback Speed", true);
    let speed_0_5 = MenuItem::new("0.5x Slow", true, None);
    let speed_1_0 = MenuItem::new("1.0x Normal", true, None);
    let speed_1_5 = MenuItem::new("1.5x Fast", true, None);
    actions.insert(
        speed_0_5.id().clone(),
        TrayAction::SetSpeed {
            monitor_id: target.cloned(),
            speed: 0.5,
        },
    );
    actions.insert(
        speed_1_0.id().clone(),
        TrayAction::SetSpeed {
            monitor_id: target.cloned(),
            speed: 1.0,
        },
    );
    actions.insert(
        speed_1_5.id().clone(),
        TrayAction::SetSpeed {
            monitor_id: target.cloned(),
            speed: 1.5,
        },
    );
    let _ = speed_submenu.append(&speed_0_5);
    let _ = speed_submenu.append(&speed_1_0);
    let _ = speed_submenu.append(&speed_1_5);
    let _ = performance_submenu.append(&speed_submenu);

    let fps_submenu = Submenu::new("Target FPS", true);
    let is_native = resolved.target_fps.unwrap_or(0) == 0;
    let native_item = CheckMenuItem::new("Native (Original)", true, is_native, None);
    actions.insert(
        native_item.id().clone(),
        TrayAction::SetTargetFps {
            monitor_id: target.cloned(),
            target_fps: None,
        },
    );
    let _ = fps_submenu.append(&native_item);

    for &(fps_val, label) in get_fps_options(native_fps) {
        let is_checked = resolved.target_fps == Some(fps_val);
        let item = CheckMenuItem::new(label, true, is_checked, None);
        actions.insert(
            item.id().clone(),
            TrayAction::SetTargetFps {
                monitor_id: target.cloned(),
                target_fps: Some(fps_val),
            },
        );
        let _ = fps_submenu.append(&item);
    }
    let _ = performance_submenu.append(&fps_submenu);

    parent.append(&performance_submenu);
}

fn append_display_mode_submenu(
    parent: &MenuParent<'_>,
    current_mode: DisplayMode,
    actions: &mut HashMap<MenuId, TrayAction>,
) {
    let mode_submenu = Submenu::new("Display Mode", true);

    let pm_item = CheckMenuItem::new(
        "Per-Monitor",
        true,
        current_mode == DisplayMode::PerMonitor,
        None,
    );
    actions.insert(
        pm_item.id().clone(),
        TrayAction::SetDisplayMode(DisplayMode::PerMonitor),
    );
    let _ = mode_submenu.append(&pm_item);

    let clone_item = CheckMenuItem::new("Clone", true, current_mode == DisplayMode::Clone, None);
    actions.insert(
        clone_item.id().clone(),
        TrayAction::SetDisplayMode(DisplayMode::Clone),
    );
    let _ = mode_submenu.append(&clone_item);

    let span_item = CheckMenuItem::new("Span", true, current_mode == DisplayMode::Span, None);
    actions.insert(
        span_item.id().clone(),
        TrayAction::SetDisplayMode(DisplayMode::Span),
    );
    let _ = mode_submenu.append(&span_item);

    parent.append(&mode_submenu);
}

pub fn build_tray_menu(
    saved_wallpapers_dir: &Path,
    config: &AppConfig,
    monitors: &[MonitorInfo],
    native_fps_by_monitor: &BTreeMap<MonitorId, u32>,
    global_native_fps: u32,
) -> (Menu, TrayControls) {
    let menu = Menu::new();
    let mut actions = HashMap::new();
    let is_per_monitor_multi =
        monitors.len() >= 2 && config.display_mode == DisplayMode::PerMonitor;

    let target_ctx = TargetControlContext {
        saved_wallpapers_dir,
        config,
        native_fps_by_monitor,
        global_native_fps,
        is_per_monitor_multi,
    };

    if is_per_monitor_multi {
        for (idx, mon) in monitors.iter().enumerate() {
            let scale = mon.dpi * 100 / 96;
            let label = format!(
                "Display {} ({}x{} @ {}%)",
                idx + 1,
                mon.rect.width(),
                mon.rect.height(),
                scale
            );
            let mon_submenu = Submenu::new(&label, true);
            append_target_controls(
                &MenuParent::Submenu(&mon_submenu),
                Some(&mon.id),
                &target_ctx,
                &mut actions,
            );
            let _ = menu.append(&mon_submenu);
        }

        let _ = menu.append(&PredefinedMenuItem::separator());

        let pause_label = if config.is_manually_paused {
            "Resume All Wallpapers"
        } else {
            "Pause All Wallpapers"
        };
        let global_pause_item = MenuItem::new(pause_label, true, None);
        actions.insert(
            global_pause_item.id().clone(),
            TrayAction::TogglePause(None),
        );
        let _ = menu.append(&global_pause_item);
    } else {
        append_target_controls(&MenuParent::Menu(&menu), None, &target_ctx, &mut actions);
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    append_display_mode_submenu(&MenuParent::Menu(&menu), config.display_mode, &mut actions);

    let _ = menu.append(&PredefinedMenuItem::separator());

    let check_updates_item = MenuItem::new("Check for Updates...", true, None);
    actions.insert(check_updates_item.id().clone(), TrayAction::CheckUpdates);
    let _ = menu.append(&check_updates_item);

    let autostart_enabled = is_autostart_enabled();
    let autostart_item = CheckMenuItem::new("Start with Windows", true, autostart_enabled, None);
    actions.insert(autostart_item.id().clone(), TrayAction::ToggleAutostart);
    let _ = menu.append(&autostart_item);

    let uninstall_item = MenuItem::new("Uninstall ArcPaper...", true, None);
    actions.insert(uninstall_item.id().clone(), TrayAction::Uninstall);
    let _ = menu.append(&uninstall_item);

    let exit_item = MenuItem::new("Exit ArcPaper", true, None);
    actions.insert(exit_item.id().clone(), TrayAction::Exit);
    let _ = menu.append(&exit_item);

    let controls = TrayControls {
        actions,
        autostart_item,
    };

    (menu, controls)
}

/// Builds the tray icon with up to 10 retries (500ms apart) while Windows shell tray initializes.
pub fn build_initial_tray_icon(
    saved_wallpapers_dir: &Path,
    config: &AppConfig,
    monitors: &[MonitorInfo],
    native_fps_by_monitor: &BTreeMap<MonitorId, u32>,
    global_native_fps: u32,
) -> (TrayIcon, TrayControls) {
    let mut built = None;
    for attempt in 1..=10 {
        let (menu, new_ctrl) = build_tray_menu(
            saved_wallpapers_dir,
            config,
            monitors,
            native_fps_by_monitor,
            global_native_fps,
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
    config: &AppConfig,
    monitors: &[MonitorInfo],
    native_fps_by_monitor: &BTreeMap<MonitorId, u32>,
    global_native_fps: u32,
) -> TrayControls {
    let (new_menu, new_controls) = build_tray_menu(
        saved_wallpapers_dir,
        config,
        monitors,
        native_fps_by_monitor,
        global_native_fps,
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

    #[test]
    fn test_tray_menu_builds_actions_for_single_and_multi_monitor_modes() {
        let temp_dir = std::env::temp_dir().join("arc_paper_tray_test_red");
        let _ = std::fs::create_dir_all(&temp_dir);

        let mon1 = crate::display::MonitorInfo {
            id: crate::display::MonitorId("MON_1".into()),
            friendly_name: "Display 1".into(),
            gdi_name: r"\\.\DISPLAY1".into(),
            rect: crate::display::DisplayRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: true,
        };
        let mon2 = crate::display::MonitorInfo {
            id: crate::display::MonitorId("MON_2".into()),
            friendly_name: "Display 2".into(),
            gdi_name: r"\\.\DISPLAY2".into(),
            rect: crate::display::DisplayRect {
                left: 1920,
                top: 0,
                right: 3840,
                bottom: 1080,
            },
            dpi: 144,
            is_primary: false,
        };

        let mut config = crate::app::config::AppConfig::default();
        let mut fps_map = std::collections::BTreeMap::new();
        fps_map.insert(mon1.id.clone(), 60);
        fps_map.insert(mon2.id.clone(), 120);

        // Case 1: Single monitor
        let single_mon = vec![mon1.clone()];
        let (_menu_1, controls_1) = build_tray_menu(&temp_dir, &config, &single_mon, &fps_map, 60);

        // Verify DisplayMode options exist
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a
                    == TrayAction::SetDisplayMode(crate::app::config::DisplayMode::PerMonitor))
        );
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::SetDisplayMode(crate::app::config::DisplayMode::Clone))
        );
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::SetDisplayMode(crate::app::config::DisplayMode::Span))
        );

        // In single monitor: global controls target None, no Some(MonitorId)
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::UploadWallpaper(None))
        );
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::TogglePause(None))
        );
        assert!(
            !controls_1
                .actions
                .values()
                .any(|a| matches!(a, TrayAction::UploadWallpaper(Some(_))))
        );

        // Case 2: Multi-monitor Span mode
        config.display_mode = crate::app::config::DisplayMode::Span;
        let multi_mon = vec![mon1.clone(), mon2.clone()];
        let (_menu_span, controls_span) =
            build_tray_menu(&temp_dir, &config, &multi_mon, &fps_map, 60);

        // In Span mode: global controls target None, no per-monitor submenus
        assert!(
            controls_span
                .actions
                .values()
                .any(|a| *a == TrayAction::UploadWallpaper(None))
        );
        assert!(
            controls_span
                .actions
                .values()
                .any(|a| *a == TrayAction::TogglePause(None))
        );
        assert!(
            !controls_span
                .actions
                .values()
                .any(|a| matches!(a, TrayAction::UploadWallpaper(Some(_))))
        );

        // Case 3: Multi-monitor PerMonitor mode
        config.display_mode = crate::app::config::DisplayMode::PerMonitor;
        let (_menu_pm, controls_pm) = build_tray_menu(&temp_dir, &config, &multi_mon, &fps_map, 60);

        // In PerMonitor mode: per-monitor controls target Some(mon1.id) and Some(mon2.id)
        assert!(
            controls_pm
                .actions
                .values()
                .any(|a| *a == TrayAction::UploadWallpaper(Some(mon1.id.clone())))
        );
        assert!(
            controls_pm
                .actions
                .values()
                .any(|a| *a == TrayAction::UploadWallpaper(Some(mon2.id.clone())))
        );
        assert!(
            controls_pm
                .actions
                .values()
                .any(|a| *a == TrayAction::TogglePause(Some(mon1.id.clone())))
        );
        assert!(
            controls_pm
                .actions
                .values()
                .any(|a| *a == TrayAction::TogglePause(Some(mon2.id.clone())))
        );
        // Global pause action also exists
        assert!(
            controls_pm
                .actions
                .values()
                .any(|a| *a == TrayAction::TogglePause(None))
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
