//! System tray icon rendering and hierarchical menu builder.

use super::PauseReason;
use super::autostart::{disable_autostart, enable_autostart, is_autostart_enabled};
use super::config::{AppConfig, DisplayMode, PowerConfig, ResolvedMonitorConfig, save_config};
use super::event_host::ArcPaperEventHost;
use super::log::{LogLevel, log_msg, log_with_level};
use super::paths::list_mp4_files;
use super::uninstall::perform_uninstall;
use super::updater::check_for_updates;
use crate::arc_player::WallpaperManager;
use crate::display::{MonitorId, MonitorInfo, diff_topologies, enumerate_monitors};
use crate::power::BatteryPolicy;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows::Win32::Foundation::HWND;

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
    SetBatteryPolicy(BatteryPolicy),
    TogglePauseOnLock,
    TogglePauseOnRemote,
    TogglePauseOnDisplayOff,
    ToggleExclusiveFullscreen,
    ToggleReduceMotion,
    ToggleAdaptiveFps,
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

fn append_power_submenu(
    parent: &MenuParent<'_>,
    power: &PowerConfig,
    actions: &mut HashMap<MenuId, TrayAction>,
) {
    let power_submenu = Submenu::new("Power", true);

    let always_play = CheckMenuItem::new(
        "Always Play",
        true,
        power.battery_policy == BatteryPolicy::AlwaysPlay,
        None,
    );
    actions.insert(
        always_play.id().clone(),
        TrayAction::SetBatteryPolicy(BatteryPolicy::AlwaysPlay),
    );
    let _ = power_submenu.append(&always_play);

    let on_saver = CheckMenuItem::new(
        "Pause on Battery Saver",
        true,
        power.battery_policy == BatteryPolicy::PauseOnBatterySaver,
        None,
    );
    actions.insert(
        on_saver.id().clone(),
        TrayAction::SetBatteryPolicy(BatteryPolicy::PauseOnBatterySaver),
    );
    let _ = power_submenu.append(&on_saver);

    let on_battery = CheckMenuItem::new(
        "Pause on Battery",
        true,
        power.battery_policy == BatteryPolicy::PauseOnBattery,
        None,
    );
    actions.insert(
        on_battery.id().clone(),
        TrayAction::SetBatteryPolicy(BatteryPolicy::PauseOnBattery),
    );
    let _ = power_submenu.append(&on_battery);

    let _ = power_submenu.append(&PredefinedMenuItem::separator());

    let pause_lock = CheckMenuItem::new("Pause on Screen Lock", true, power.pause_on_lock, None);
    actions.insert(pause_lock.id().clone(), TrayAction::TogglePauseOnLock);
    let _ = power_submenu.append(&pause_lock);

    let pause_remote =
        CheckMenuItem::new("Pause on Remote Desktop", true, power.pause_on_remote, None);
    actions.insert(pause_remote.id().clone(), TrayAction::TogglePauseOnRemote);
    let _ = power_submenu.append(&pause_remote);

    let pause_display = CheckMenuItem::new(
        "Pause on Display Off",
        true,
        power.pause_on_display_off,
        None,
    );
    actions.insert(
        pause_display.id().clone(),
        TrayAction::TogglePauseOnDisplayOff,
    );
    let _ = power_submenu.append(&pause_display);

    let pause_fullscreen = CheckMenuItem::new(
        "Pause on Exclusive Fullscreen",
        true,
        power.detect_exclusive_fullscreen,
        None,
    );
    actions.insert(
        pause_fullscreen.id().clone(),
        TrayAction::ToggleExclusiveFullscreen,
    );
    let _ = power_submenu.append(&pause_fullscreen);

    let reduce_motion = CheckMenuItem::new(
        "Respect Reduce Motion",
        true,
        power.follow_reduce_motion,
        None,
    );
    actions.insert(reduce_motion.id().clone(), TrayAction::ToggleReduceMotion);
    let _ = power_submenu.append(&reduce_motion);

    let adaptive_fps = CheckMenuItem::new(
        "Adaptive 30 FPS on Low Battery",
        true,
        power.adaptive_fps,
        None,
    );
    actions.insert(adaptive_fps.id().clone(), TrayAction::ToggleAdaptiveFps);
    let _ = power_submenu.append(&adaptive_fps);

    parent.append(&power_submenu);
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
    append_power_submenu(&MenuParent::Menu(&menu), &config.power, &mut actions);

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

/// Formats the tray tooltip string, adding dominant pause status when paused (FR-308).
pub fn format_tray_tooltip(is_paused: bool, global_reasons: PauseReason) -> String {
    if is_paused {
        if let Some(label) = global_reasons.dominant_reason_label() {
            format!("ArcPaper - Paused ({label})")
        } else {
            "ArcPaper - Paused".to_string()
        }
    } else {
        "ArcPaper".to_string()
    }
}

/// Updates the tray icon tooltip with the current pause state and dominant reason.
pub fn update_tray_tooltip(
    tray_icon: &tray_icon::TrayIcon,
    is_paused: bool,
    global_reasons: PauseReason,
) {
    let tooltip = format_tray_tooltip(is_paused, global_reasons);
    let _ = tray_icon.set_tooltip(Some(tooltip));
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
    let initial_tooltip = format_tray_tooltip(
        config.is_manually_paused,
        if config.is_manually_paused {
            PauseReason::MANUAL
        } else {
            PauseReason::empty()
        },
    );
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
            .with_tooltip(&initial_tooltip)
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

pub fn log_fps_limit(target_fps: Option<u32>) {
    match target_fps {
        Some(fps) => log_msg(&format!(
            "[ArcPaper] Applying FPS pacing limit: {} FPS",
            fps
        )),
        None => log_msg("[ArcPaper] Resetting FPS pacing to Native frame rate"),
    }
}

pub fn compute_effective_paused_map(
    monitors: &[MonitorInfo],
    config: &AppConfig,
    coverage_map: &BTreeMap<MonitorId, bool>,
) -> BTreeMap<MonitorId, bool> {
    compute_effective_paused_map_with_reasons(monitors, config, coverage_map, PauseReason::empty())
}

/// Computes effective pause map combining per-monitor coverage with global pause reasons.
pub fn compute_effective_paused_map_with_reasons(
    monitors: &[MonitorInfo],
    config: &AppConfig,
    coverage_map: &BTreeMap<MonitorId, bool>,
    global_reasons: PauseReason,
) -> BTreeMap<MonitorId, bool> {
    let mut map = BTreeMap::new();
    for mon in monitors {
        let covered = coverage_map.get(&mon.id).copied().unwrap_or(false);
        let resolved = config.resolve_for_monitor(&mon.id);
        let mut global = global_reasons;
        if resolved.is_manually_paused {
            global.insert(PauseReason::MANUAL);
        }
        let monitor = if covered {
            PauseReason::COVERED
        } else {
            PauseReason::empty()
        };
        let effective = crate::app::pause::resolve_effective_monitor_pause(global, monitor);
        map.insert(mon.id.clone(), effective);
    }
    map
}

/// Applies effective pause states across all monitors and trims working set if all monitors are paused.
pub fn sync_effective_pause(
    manager: &mut Option<WallpaperManager>,
    monitors: &[MonitorInfo],
    config: &AppConfig,
    coverage: &BTreeMap<MonitorId, bool>,
    global_reasons: PauseReason,
) -> BTreeMap<MonitorId, bool> {
    let effective =
        compute_effective_paused_map_with_reasons(monitors, config, coverage, global_reasons);
    if let Some(mgr) = manager.as_mut() {
        mgr.update_pause_states(config, &effective);
    }
    if !effective.is_empty() && effective.values().all(|&p| p) {
        crate::arc_player::AutoPause::trim_working_set();
    }
    effective
}

/// Diagnostic and telemetry options for pause transition logging.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagPauseConfig {
    pub latency_ms: Option<u64>,
    pub is_diag: bool,
}

impl DiagPauseConfig {
    pub fn new(latency_ms: Option<u64>, is_diag: bool) -> Self {
        Self {
            latency_ms,
            is_diag,
        }
    }
}

/// Applies effective pause states across all monitors, logs any state transitions,
/// and trims working set if all monitors are paused.
pub fn sync_and_log_effective_pause(
    manager: &mut Option<WallpaperManager>,
    monitors: &[MonitorInfo],
    config: &AppConfig,
    coverage: &BTreeMap<MonitorId, bool>,
    global_reasons: PauseReason,
    prev_effective: &mut BTreeMap<MonitorId, bool>,
    diag: DiagPauseConfig,
) -> BTreeMap<MonitorId, bool> {
    let effective = sync_effective_pause(manager, monitors, config, coverage, global_reasons);
    for mon in monitors {
        let was_paused = prev_effective.get(&mon.id).copied().unwrap_or(false);
        let is_paused = effective.get(&mon.id).copied().unwrap_or(false);
        if was_paused != is_paused {
            let reason_label = if is_paused {
                let resolved = config.resolve_for_monitor(&mon.id);
                let mut reasons = global_reasons;
                if resolved.is_manually_paused {
                    reasons.insert(PauseReason::MANUAL);
                }
                if coverage.get(&mon.id).copied().unwrap_or(false) {
                    reasons.insert(PauseReason::COVERED);
                }
                reasons.dominant_reason_label()
            } else {
                None
            };
            crate::app::log::log_pause_transition(
                &mon.friendly_name,
                is_paused,
                reason_label,
                diag.latency_ms,
                diag.is_diag,
            );
        }
    }
    *prev_effective = effective.clone();
    effective
}

pub fn collect_native_fps(
    manager: &WallpaperManager,
    monitors: &[MonitorInfo],
    fallback_fps: u32,
) -> (BTreeMap<MonitorId, u32>, u32) {
    let mut fps_map = BTreeMap::new();
    let mut max_fps = fallback_fps;
    for mon in monitors {
        if let Some(fps) = manager.native_fps_for_monitor(&mon.id) {
            fps_map.insert(mon.id.clone(), fps);
            if fps > max_fps {
                max_fps = fps;
            }
        } else {
            fps_map.insert(mon.id.clone(), fallback_fps);
        }
    }
    (fps_map, max_fps)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ManagerSyncMode {
    ApplyTopology,
    PauseOnly,
    RebuildTrayOnly,
}

impl ManagerSyncMode {
    pub fn requires_topology_sync(&self) -> bool {
        matches!(self, Self::ApplyTopology)
    }

    pub fn requires_manager_sync(&self) -> bool {
        matches!(self, Self::ApplyTopology | Self::PauseOnly)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TrayOutcome {
    Continue,
    Exit,
}

#[allow(clippy::too_many_arguments)]
pub fn sync_manager_and_rebuild_tray(
    manager: &mut Option<WallpaperManager>,
    workerw_hwnd: HWND,
    current_monitors: &[MonitorInfo],
    config: &AppConfig,
    current_coverage_map: &BTreeMap<MonitorId, bool>,
    saved_wallpapers_dir: &Path,
    tray_icon: &tray_icon::TrayIcon,
    controls: &mut TrayControls,
    sync_mode: ManagerSyncMode,
) {
    sync_manager_and_rebuild_tray_with_reasons(
        manager,
        workerw_hwnd,
        current_monitors,
        config,
        current_coverage_map,
        saved_wallpapers_dir,
        tray_icon,
        controls,
        sync_mode,
        PauseReason::empty(),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn sync_manager_and_rebuild_tray_with_reasons(
    manager: &mut Option<WallpaperManager>,
    workerw_hwnd: HWND,
    current_monitors: &[MonitorInfo],
    config: &AppConfig,
    current_coverage_map: &BTreeMap<MonitorId, bool>,
    saved_wallpapers_dir: &Path,
    tray_icon: &tray_icon::TrayIcon,
    controls: &mut TrayControls,
    sync_mode: ManagerSyncMode,
    global_reasons: PauseReason,
) {
    if sync_mode.requires_manager_sync() {
        let effective_paused = compute_effective_paused_map_with_reasons(
            current_monitors,
            config,
            current_coverage_map,
            global_reasons,
        );
        if let Some(mgr) = manager.as_mut() {
            if sync_mode.requires_topology_sync() {
                mgr.apply_topology_and_config(
                    workerw_hwnd,
                    current_monitors,
                    config,
                    &effective_paused,
                    saved_wallpapers_dir,
                );
            } else {
                mgr.update_pause_states(config, &effective_paused);
            }
        }
    }

    let (fps_map, global_fps) = if let Some(mgr) = manager.as_ref() {
        collect_native_fps(mgr, current_monitors, 60)
    } else {
        (BTreeMap::new(), 60)
    };
    *controls = rebuild_tray(
        tray_icon,
        saved_wallpapers_dir,
        config,
        current_monitors,
        &fps_map,
        global_fps,
    );

    let is_paused = config.is_manually_paused || !global_reasons.is_empty();
    let combined_reasons = if config.is_manually_paused {
        global_reasons | PauseReason::MANUAL
    } else {
        global_reasons
    };
    update_tray_tooltip(tray_icon, is_paused, combined_reasons);
}

#[allow(clippy::too_many_arguments)]
pub fn handle_topology_change_if_needed(
    current_monitors: &mut Vec<MonitorInfo>,
    monitors_snapshot: &Arc<RwLock<Vec<MonitorInfo>>>,
    manager: &mut Option<WallpaperManager>,
    workerw_hwnd: HWND,
    wp_state: &std::rc::Rc<std::cell::RefCell<AppConfig>>,
    current_coverage_map: &BTreeMap<MonitorId, bool>,
    saved_wallpapers_dir: &Path,
    tray_icon: &tray_icon::TrayIcon,
    controls: &mut TrayControls,
) {
    let next_monitors = enumerate_monitors();
    let diff = diff_topologies(current_monitors, &next_monitors);
    let monitor_count_mismatch = manager
        .as_ref()
        .map(|m| m.monitor_count() != next_monitors.len())
        .unwrap_or(false);

    if diff.has_changes() || monitor_count_mismatch {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::EndMenu();
        }
        log_msg(&format!(
            "[ArcPaper] Display topology changed: added={}, removed={}, changed={}, unchanged={}. Updating layout...",
            diff.added.len(),
            diff.removed.len(),
            diff.changed.len(),
            diff.unchanged.len()
        ));
        *current_monitors = next_monitors.clone();
        if let Ok(mut snap) = monitors_snapshot.write() {
            *snap = next_monitors;
        }
        sync_manager_and_rebuild_tray(
            manager,
            workerw_hwnd,
            current_monitors,
            &wp_state.borrow(),
            current_coverage_map,
            saved_wallpapers_dir,
            tray_icon,
            controls,
            ManagerSyncMode::ApplyTopology,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub fn handle_tray_action(
    action: TrayAction,
    manager: &mut Option<WallpaperManager>,
    workerw_hwnd: HWND,
    current_monitors: &[MonitorInfo],
    wp_state: &std::rc::Rc<std::cell::RefCell<AppConfig>>,
    current_coverage_map: &BTreeMap<MonitorId, bool>,
    saved_wallpapers_dir: &Path,
    config_path: &Path,
    tray_icon: &tray_icon::TrayIcon,
    controls: &mut TrayControls,
    mut event_host: Option<&mut ArcPaperEventHost>,
) -> TrayOutcome {
    match action {
        TrayAction::UploadWallpaper(target) => {
            if let Some(source_path) = rfd::FileDialog::new()
                .add_filter("MP4 Video (*.mp4)", &["mp4"])
                .set_title("Select MP4 Video File")
                .pick_file()
                && let Some(file_name) = source_path.file_name().and_then(|n| n.to_str())
            {
                let dest_path = saved_wallpapers_dir.join(file_name);
                if let Err(e) = std::fs::copy(&source_path, &dest_path) {
                    log_with_level(LogLevel::Error, &format!("[ArcPaper] Copy failed: {e:?}"));
                } else {
                    let mut state = wp_state.borrow_mut();
                    match &target {
                        Some(mid) => {
                            let entry = state.monitors.entry(mid.as_str().to_string()).or_default();
                            entry.active_video = Some(file_name.to_string());
                            entry.is_manually_paused = Some(false);
                        }
                        None => {
                            state.active_video = Some(file_name.to_string());
                            state.is_manually_paused = false;
                        }
                    }
                    save_config(config_path, &state);
                    drop(state);

                    sync_manager_and_rebuild_tray(
                        manager,
                        workerw_hwnd,
                        current_monitors,
                        &wp_state.borrow(),
                        current_coverage_map,
                        saved_wallpapers_dir,
                        tray_icon,
                        controls,
                        ManagerSyncMode::ApplyTopology,
                    );
                }
            }
        }

        TrayAction::SelectWallpaper { monitor_id, file } => {
            let mut state = wp_state.borrow_mut();
            match &monitor_id {
                Some(mid) => {
                    let entry = state.monitors.entry(mid.as_str().to_string()).or_default();
                    entry.active_video = Some(file);
                    entry.is_manually_paused = Some(false);
                }
                None => {
                    state.active_video = Some(file);
                    state.is_manually_paused = false;
                }
            }
            save_config(config_path, &state);
            drop(state);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::ApplyTopology,
            );
        }

        TrayAction::SetDisplayMode(new_mode) => {
            let mut state = wp_state.borrow_mut();
            state.display_mode = new_mode;
            save_config(config_path, &state);
            drop(state);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::ApplyTopology,
            );
        }

        TrayAction::TogglePause(target) => {
            let mut state = wp_state.borrow_mut();
            match &target {
                Some(mid) => {
                    let currently_paused = state.resolve_for_monitor(mid).is_manually_paused;
                    let entry = state.monitors.entry(mid.as_str().to_string()).or_default();
                    entry.is_manually_paused = Some(!currently_paused);
                }
                None => {
                    let next = !state.is_manually_paused;
                    state.is_manually_paused = next;
                    for ov in state.monitors.values_mut() {
                        ov.is_manually_paused = None;
                    }
                }
            }
            save_config(config_path, &state);
            drop(state);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::PauseOnly,
            );
        }

        TrayAction::SetFit { monitor_id, fit } => {
            let mut state = wp_state.borrow_mut();
            match &monitor_id {
                Some(mid) => {
                    let ov = state.monitors.entry(mid.as_str().to_string()).or_default();
                    ov.fit_mode = Some(fit.to_string());
                }
                None => {
                    state.fit_mode = fit.to_string();
                }
            }
            save_config(config_path, &state);
            drop(state);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::ApplyTopology,
            );
        }

        TrayAction::SetBrightness {
            monitor_id,
            brightness,
        } => {
            let mut state = wp_state.borrow_mut();
            match &monitor_id {
                Some(mid) => {
                    let ov = state.monitors.entry(mid.as_str().to_string()).or_default();
                    ov.brightness = Some(brightness);
                }
                None => {
                    state.brightness = brightness;
                }
            }
            save_config(config_path, &state);
            drop(state);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::ApplyTopology,
            );
        }

        TrayAction::SetSpeed { monitor_id, speed } => {
            let mut state = wp_state.borrow_mut();
            match &monitor_id {
                Some(mid) => {
                    let ov = state.monitors.entry(mid.as_str().to_string()).or_default();
                    ov.speed = Some(speed);
                }
                None => {
                    state.speed = speed;
                }
            }
            save_config(config_path, &state);
            drop(state);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::ApplyTopology,
            );
        }

        TrayAction::SetTargetFps {
            monitor_id,
            target_fps,
        } => {
            let mut state = wp_state.borrow_mut();
            match &monitor_id {
                Some(mid) => {
                    let ov = state.monitors.entry(mid.as_str().to_string()).or_default();
                    ov.target_fps = target_fps;
                }
                None => {
                    state.target_fps = target_fps;
                }
            }
            log_fps_limit(target_fps);
            save_config(config_path, &state);
            drop(state);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::ApplyTopology,
            );
        }

        TrayAction::SetBatteryPolicy(policy) => {
            let mut state = wp_state.borrow_mut();
            state.power.battery_policy = policy;
            save_config(config_path, &state);
            drop(state);

            if let Some(ref mut host) = event_host {
                host.set_battery_policy(policy);
            }

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::RebuildTrayOnly,
            );
        }

        TrayAction::TogglePauseOnLock => {
            let mut state = wp_state.borrow_mut();
            state.power.pause_on_lock = !state.power.pause_on_lock;
            let pause_on_lock = state.power.pause_on_lock;
            save_config(config_path, &state);
            drop(state);

            if let Some(ref mut host) = event_host {
                host.set_pause_on_lock(pause_on_lock);
            }

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::RebuildTrayOnly,
            );
        }

        TrayAction::TogglePauseOnRemote => {
            let mut state = wp_state.borrow_mut();
            state.power.pause_on_remote = !state.power.pause_on_remote;
            let pause_on_remote = state.power.pause_on_remote;
            save_config(config_path, &state);
            drop(state);

            if let Some(ref mut host) = event_host {
                host.set_pause_on_remote(pause_on_remote);
            }

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::RebuildTrayOnly,
            );
        }

        TrayAction::TogglePauseOnDisplayOff => {
            let mut state = wp_state.borrow_mut();
            state.power.pause_on_display_off = !state.power.pause_on_display_off;
            let pause_on_display_off = state.power.pause_on_display_off;
            save_config(config_path, &state);
            drop(state);

            if let Some(ref mut host) = event_host {
                host.set_pause_on_display_off(pause_on_display_off);
            }

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::RebuildTrayOnly,
            );
        }

        TrayAction::ToggleExclusiveFullscreen => {
            let mut state = wp_state.borrow_mut();
            state.power.detect_exclusive_fullscreen = !state.power.detect_exclusive_fullscreen;
            let detect = state.power.detect_exclusive_fullscreen;
            save_config(config_path, &state);
            drop(state);

            crate::app::winevent::set_detect_exclusive_fullscreen(detect);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::RebuildTrayOnly,
            );
        }

        TrayAction::ToggleReduceMotion => {
            let mut state = wp_state.borrow_mut();
            state.power.follow_reduce_motion = !state.power.follow_reduce_motion;
            let follow = state.power.follow_reduce_motion;
            save_config(config_path, &state);
            drop(state);

            if let Some(ref mut host) = event_host {
                host.set_follow_reduce_motion(follow);
            }

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::RebuildTrayOnly,
            );
        }

        TrayAction::ToggleAdaptiveFps => {
            let mut state = wp_state.borrow_mut();
            state.power.adaptive_fps = !state.power.adaptive_fps;
            save_config(config_path, &state);
            drop(state);

            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::RebuildTrayOnly,
            );
        }

        TrayAction::CheckUpdates => {
            std::thread::spawn(check_for_updates);
        }

        TrayAction::ToggleAutostart => {
            let res = if is_autostart_enabled() {
                disable_autostart()
            } else {
                enable_autostart()
            };
            if let Err(e) = res {
                log_with_level(
                    LogLevel::Error,
                    &format!("[ArcPaper] Auto-start error: {e:?}"),
                );
            }
            controls.autostart_item.set_checked(is_autostart_enabled());
            sync_manager_and_rebuild_tray(
                manager,
                workerw_hwnd,
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
                saved_wallpapers_dir,
                tray_icon,
                controls,
                ManagerSyncMode::RebuildTrayOnly,
            );
        }

        TrayAction::Uninstall => {
            perform_uninstall();
        }

        TrayAction::Exit => {
            log_msg("[ArcPaper] Exit requested. Shutting down cleanly...");
            return TrayOutcome::Exit;
        }
    }
    TrayOutcome::Continue
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

        // Verify Power actions exist
        assert!(
            controls_1.actions.values().any(
                |a| *a == TrayAction::SetBatteryPolicy(crate::power::BatteryPolicy::AlwaysPlay)
            )
        );
        assert!(controls_1.actions.values().any(|a| *a
            == TrayAction::SetBatteryPolicy(crate::power::BatteryPolicy::PauseOnBatterySaver)));
        assert!(controls_1.actions.values().any(
            |a| *a == TrayAction::SetBatteryPolicy(crate::power::BatteryPolicy::PauseOnBattery)
        ));
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::TogglePauseOnLock)
        );
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::TogglePauseOnRemote)
        );
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::TogglePauseOnDisplayOff)
        );
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::ToggleExclusiveFullscreen)
        );
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::ToggleReduceMotion)
        );
        assert!(
            controls_1
                .actions
                .values()
                .any(|a| *a == TrayAction::ToggleAdaptiveFps)
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

    #[test]
    fn test_manager_sync_mode_methods() {
        assert!(ManagerSyncMode::ApplyTopology.requires_topology_sync());
        assert!(!ManagerSyncMode::PauseOnly.requires_topology_sync());
        assert!(!ManagerSyncMode::RebuildTrayOnly.requires_topology_sync());

        assert!(ManagerSyncMode::ApplyTopology.requires_manager_sync());
        assert!(ManagerSyncMode::PauseOnly.requires_manager_sync());
        assert!(!ManagerSyncMode::RebuildTrayOnly.requires_manager_sync());
    }

    #[test]
    fn test_compute_effective_paused_map_logic() {
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
        let monitors = vec![mon1.clone()];
        let mut config = crate::app::config::AppConfig::default();
        let mut coverage = BTreeMap::new();

        // Not covered, not manually paused
        let map = compute_effective_paused_map(&monitors, &config, &coverage);
        assert_eq!(map.get(&mon1.id), Some(&false));

        // Covered
        coverage.insert(mon1.id.clone(), true);
        let map = compute_effective_paused_map(&monitors, &config, &coverage);
        assert_eq!(map.get(&mon1.id), Some(&true));

        // Not covered, manually paused
        coverage.insert(mon1.id.clone(), false);
        config.is_manually_paused = true;
        let map = compute_effective_paused_map(&monitors, &config, &coverage);
        assert_eq!(map.get(&mon1.id), Some(&true));

        // Global pause reason: Battery Saver
        config.is_manually_paused = false;
        let map_global = compute_effective_paused_map_with_reasons(
            &monitors,
            &config,
            &coverage,
            PauseReason::BATTERY_SAVER,
        );
        assert_eq!(map_global.get(&mon1.id), Some(&true));

        // Global pause reason: Session Locked
        let map_locked = compute_effective_paused_map_with_reasons(
            &monitors,
            &config,
            &coverage,
            PauseReason::SESSION_LOCKED,
        );
        assert_eq!(map_locked.get(&mon1.id), Some(&true));

        // Global pause reason: Empty
        let map_empty = compute_effective_paused_map_with_reasons(
            &monitors,
            &config,
            &coverage,
            PauseReason::empty(),
        );
        assert_eq!(map_empty.get(&mon1.id), Some(&false));
    }

    #[test]
    fn test_format_tray_tooltip_dominant_reasons() {
        assert_eq!(format_tray_tooltip(false, PauseReason::empty()), "ArcPaper");
        assert_eq!(
            format_tray_tooltip(true, PauseReason::empty()),
            "ArcPaper - Paused"
        );
        assert_eq!(
            format_tray_tooltip(true, PauseReason::BATTERY_SAVER),
            "ArcPaper - Paused (Battery Saver Active)"
        );
        assert_eq!(
            format_tray_tooltip(true, PauseReason::BATTERY),
            "ArcPaper - Paused (Running on Battery)"
        );
        assert_eq!(
            format_tray_tooltip(true, PauseReason::SESSION_LOCKED),
            "ArcPaper - Paused (Session Locked)"
        );
        assert_eq!(
            format_tray_tooltip(true, PauseReason::REMOTE_SESSION),
            "ArcPaper - Paused (Remote Desktop Active)"
        );
        assert_eq!(
            format_tray_tooltip(true, PauseReason::DISPLAY_OFF),
            "ArcPaper - Paused (Display Turned Off)"
        );
        assert_eq!(
            format_tray_tooltip(true, PauseReason::MANUAL),
            "ArcPaper - Paused (Manually Paused)"
        );
        // Precedence: SUSPENDED > BATTERY_SAVER
        assert_eq!(
            format_tray_tooltip(true, PauseReason::SUSPENDED | PauseReason::BATTERY_SAVER),
            "ArcPaper - Paused (System Suspended)"
        );
    }

    #[test]
    fn test_sync_and_log_effective_pause_transitions() {
        let mon = MonitorInfo {
            id: MonitorId::from("mon-1"),
            friendly_name: "Primary Display".to_string(),
            gdi_name: r"\\.\DISPLAY1".to_string(),
            rect: crate::display::DisplayRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: true,
        };
        let monitors = vec![mon.clone()];
        let config = AppConfig::default();
        let mut coverage = BTreeMap::new();
        let mut prev_effective = BTreeMap::new();
        let mut mgr = None;

        // Transition: Unpaused -> Paused (Window Covered)
        coverage.insert(mon.id.clone(), true);
        let eff = sync_and_log_effective_pause(
            &mut mgr,
            &monitors,
            &config,
            &coverage,
            PauseReason::empty(),
            &mut prev_effective,
            DiagPauseConfig::new(Some(45), true),
        );
        assert_eq!(eff.get(&mon.id), Some(&true));
        assert_eq!(prev_effective.get(&mon.id), Some(&true));

        // Transition: Paused -> Unpaused (Desktop Visible)
        coverage.insert(mon.id.clone(), false);
        let eff2 = sync_and_log_effective_pause(
            &mut mgr,
            &monitors,
            &config,
            &coverage,
            PauseReason::empty(),
            &mut prev_effective,
            DiagPauseConfig::new(Some(35), true),
        );
        assert_eq!(eff2.get(&mon.id), Some(&false));
        assert_eq!(prev_effective.get(&mon.id), Some(&false));
    }
}
