#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod windows_app {
    use arc_paper::app::PauseReason;
    use arc_paper::app::autopause_monitor::spawn_multi_monitor_thread;
    use arc_paper::app::config::{AppConfig, load_config, save_config};
    use arc_paper::app::event_host::{ArcPaperEventHost, HostEvent};
    use arc_paper::app::log::{
        DEFAULT_MAX_LOG_BACKUPS, DEFAULT_MAX_LOG_BYTES, LogLevel, init_logger, log_msg,
        log_with_level,
    };
    use arc_paper::app::paths::{
        CliArgs, acquire_single_instance_mutex_or_exit, configure_process_and_dpi, get_app_dir,
        get_config_path, get_saved_wallpapers_dir, list_mp4_files,
    };
    use arc_paper::app::tray::{
        DiagPauseConfig, TrayOutcome, build_initial_tray_icon, collect_native_fps,
        compute_effective_paused_map_with_reasons, handle_topology_change_if_needed,
        handle_tray_action, sync_and_log_effective_pause, update_tray_tooltip,
    };
    use arc_paper::arc_player::WallpaperManager;
    use arc_paper::display::topology::DisplayDebounce;
    use arc_paper::display::{MonitorId, enumerate_monitors};
    use arc_paper::workerw;

    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;
    use tao::event::Event;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::menu::MenuEvent;

    enum UserEvent {
        TrayMenu(MenuEvent),
        AutoPauseMap(BTreeMap<MonitorId, bool>),
        Host(HostEvent),
    }

    pub fn run() {
        let cli_args = CliArgs::parse();
        configure_process_and_dpi();
        init_logger(
            get_app_dir(),
            DEFAULT_MAX_LOG_BYTES,
            DEFAULT_MAX_LOG_BACKUPS,
        );

        let _instance_mutex = acquire_single_instance_mutex_or_exit();

        std::panic::set_hook(Box::new(|info| {
            log_with_level(LogLevel::Error, &format!("[PANIC] {:?}", info));
        }));

        log_msg("[ArcPaper] Starting ArcPaper Native Wallpaper Engine");
        if cli_args.diag_pause {
            log_msg("[ArcPaper] Diagnostic pause latency logging enabled (--diag-pause)");
        }
        if cli_args.force_polling {
            log_msg("[ArcPaper] Forced polling window monitor mode requested (--force-polling)");
        }

        let saved_wallpapers_dir = get_saved_wallpapers_dir();
        let config_path = get_config_path();
        let mut workerw_hwnd = workerw::attach_virtual_desktop_surface();
        let mut current_monitors = enumerate_monitors();
        let monitors_snapshot = Arc::new(RwLock::new(current_monitors.clone()));

        let mut manager = WallpaperManager::new(workerw_hwnd)
            .map_err(|e| log_with_level(LogLevel::Error, &format!("[ArcPaper] Manager error: {e}")))
            .ok();

        let wp_state: Rc<RefCell<AppConfig>> = Rc::new(RefCell::new(load_config(&config_path)));
        let mut current_coverage_map: BTreeMap<MonitorId, bool> = BTreeMap::new();
        let mut display_debounce = DisplayDebounce::new(Duration::from_millis(200));

        let valid_video = wp_state
            .borrow()
            .active_video
            .clone()
            .filter(|name| saved_wallpapers_dir.join(name).is_file());
        let video_to_play = valid_video.or_else(|| {
            let first = list_mp4_files(&saved_wallpapers_dir).into_iter().next()?;
            wp_state.borrow_mut().active_video = Some(first.clone());
            save_config(&config_path, &wp_state.borrow());
            Some(first)
        });

        if let Some(ref name) = video_to_play {
            log_msg(&format!("[ArcPaper] Active startup wallpaper: {}", name));
        }

        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

        let proxy_event_host = event_loop.create_proxy();
        let mut current_global_reasons = PauseReason::empty();
        let mut event_host = ArcPaperEventHost::new(move |event| {
            let _ = proxy_event_host.send_event(UserEvent::Host(event));
        })
        .map_err(|e| log_with_level(LogLevel::Error, &format!("[ArcPaper] EventHost error: {e}")))
        .ok();
        if let Some(ref mut host) = event_host {
            host.update_power_config(&wp_state.borrow().power);
            current_global_reasons = host.current_global_reasons();
        }
        arc_paper::app::winevent::set_detect_exclusive_fullscreen(
            wp_state.borrow().power.detect_exclusive_fullscreen,
        );

        if !current_global_reasons.is_empty() {
            log_msg(&format!(
                "[ArcPaper] Initial global pause reasons: {:?}",
                current_global_reasons
            ));
        }

        let mut current_effective_paused = compute_effective_paused_map_with_reasons(
            &current_monitors,
            &wp_state.borrow(),
            &current_coverage_map,
            current_global_reasons,
        );

        if let Some(ref mut mgr) = manager {
            mgr.apply_topology_and_config(
                workerw_hwnd,
                &current_monitors,
                &wp_state.borrow(),
                &current_effective_paused,
                &saved_wallpapers_dir,
            );
        }

        let (fps_map, global_fps) = if let Some(ref mgr) = manager {
            collect_native_fps(mgr, &current_monitors, 60)
        } else {
            (BTreeMap::new(), 60)
        };

        let (mut tray_icon, mut controls) = build_initial_tray_icon(
            &saved_wallpapers_dir,
            &wp_state.borrow(),
            &current_monitors,
            &fps_map,
            global_fps,
        );

        let proxy_tray = event_loop.create_proxy();
        let proxy_tray_cb = proxy_tray.clone();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = proxy_tray_cb.send_event(UserEvent::TrayMenu(event));
        }));

        let proxy_autopause = event_loop.create_proxy();
        spawn_multi_monitor_thread(Arc::clone(&monitors_snapshot), move |coverage| {
            let _ = proxy_autopause.send_event(UserEvent::AutoPauseMap(coverage));
        });

        event_loop.run(move |event, _, control_flow| {
            let now = std::time::Instant::now();
            let default_deadline = now + Duration::from_millis(200);
            let next_wake = match display_debounce.next_deadline() {
                Some(d) if d < default_deadline => d,
                _ => default_deadline,
            };
            *control_flow = ControlFlow::WaitUntil(next_wake);

            let _keep_tray = &tray_icon;

            while tray_icon::TrayIconEvent::receiver().try_recv().is_ok() {}

            if display_debounce.poll_ready(now) {
                handle_topology_change_if_needed(
                    &mut current_monitors,
                    &monitors_snapshot,
                    &mut manager,
                    workerw_hwnd,
                    &wp_state,
                    &current_coverage_map,
                    &saved_wallpapers_dir,
                    &tray_icon,
                    &mut controls,
                );
            }

            match event {
                Event::UserEvent(UserEvent::AutoPauseMap(coverage_map)) => {
                    let prev_covered = current_coverage_map.values().filter(|&&c| c).count();
                    let next_covered = coverage_map.values().filter(|&&c| c).count();
                    if prev_covered != next_covered {
                        if next_covered > 0 {
                            log_msg(&format!(
                                "[ArcPaper] Smart Auto-Pause: Covering window active on {}/{} monitor(s).",
                                next_covered,
                                current_monitors.len().max(next_covered)
                            ));
                        } else {
                            log_msg("[ArcPaper] Smart Auto-Resume: Desktop visible on all monitors.");
                        }
                    }
                    current_coverage_map = coverage_map;
                    let latency_ms = arc_paper::app::winevent::take_last_transition_latency_ms();
                    sync_and_log_effective_pause(
                        &mut manager,
                        &current_monitors,
                        &wp_state.borrow(),
                        &current_coverage_map,
                        current_global_reasons,
                        &mut current_effective_paused,
                        DiagPauseConfig::new(latency_ms, cli_args.diag_pause),
                    );
                    let is_p = !current_global_reasons.is_empty() || wp_state.borrow().is_manually_paused;
                    let reasons = if wp_state.borrow().is_manually_paused { current_global_reasons | PauseReason::MANUAL } else { current_global_reasons };
                    update_tray_tooltip(&tray_icon, is_p, reasons);
                }

                Event::UserEvent(UserEvent::Host(HostEvent::DisplayTopologyDirty)) => {
                    display_debounce.trigger_at(std::time::Instant::now());
                }

                Event::UserEvent(UserEvent::Host(HostEvent::GlobalPauseReasonsChanged(reasons))) => {
                    let prev_reasons = current_global_reasons;
                    current_global_reasons = reasons;
                    if prev_reasons != current_global_reasons {
                        if let Some(label) = current_global_reasons.dominant_reason_label() {
                            log_msg(&format!(
                                "[ArcPaper] Global pause reasons changed: {:?} (Dominant: {})",
                                current_global_reasons, label
                            ));
                        } else {
                            log_msg("[ArcPaper] Global pause reasons cleared. Resuming active playback.");
                        }
                    }
                    sync_and_log_effective_pause(
                        &mut manager,
                        &current_monitors,
                        &wp_state.borrow(),
                        &current_coverage_map,
                        current_global_reasons,
                        &mut current_effective_paused,
                        DiagPauseConfig::new(None, cli_args.diag_pause),
                    );
                    let is_p = !current_global_reasons.is_empty() || wp_state.borrow().is_manually_paused;
                    let reasons = if wp_state.borrow().is_manually_paused { current_global_reasons | PauseReason::MANUAL } else { current_global_reasons };
                    update_tray_tooltip(&tray_icon, is_p, reasons);
                }

                Event::UserEvent(UserEvent::Host(HostEvent::ResumeFromSuspend)) => {
                    log_msg("[ArcPaper] System resumed from suspend. Validating D3D11 device health...");
                    current_global_reasons = event_host.as_ref().map_or_else(
                        || current_global_reasons.difference(PauseReason::SUSPENDED),
                        |h| h.current_global_reasons(),
                    );
                    let device_ok = manager
                        .as_mut()
                        .is_none_or(|m| m.validate_or_recover_devices());
                    if device_ok {
                        sync_and_log_effective_pause(
                            &mut manager,
                            &current_monitors,
                            &wp_state.borrow(),
                            &current_coverage_map,
                            current_global_reasons,
                            &mut current_effective_paused,
                            DiagPauseConfig::new(None, cli_args.diag_pause),
                        );
                    }
                }

                Event::UserEvent(UserEvent::Host(HostEvent::ExplorerRestarted)) => {
                    log_msg(
                        "[ArcPaper] Explorer restart detected (TaskbarCreated). Reattaching to WorkerW...",
                    );
                    let new_workerw = workerw::attach_virtual_desktop_surface();
                    workerw_hwnd = new_workerw;
                    let next_monitors = enumerate_monitors();
                    current_monitors = next_monitors.clone();
                    if let Ok(mut snap) = monitors_snapshot.write() {
                        *snap = next_monitors.clone();
                    }
                    if let Some(ref mut mgr) = manager {
                        mgr.reparent_to_workerw(new_workerw, &current_monitors);
                        let effective_paused = compute_effective_paused_map_with_reasons(
                            &current_monitors,
                            &wp_state.borrow(),
                            &current_coverage_map,
                            current_global_reasons,
                        );
                        current_effective_paused = effective_paused.clone();
                        mgr.apply_topology_and_config(
                            new_workerw,
                            &current_monitors,
                            &wp_state.borrow(),
                            &effective_paused,
                            &saved_wallpapers_dir,
                        );
                    }
                    let (fps_map, global_fps) = if let Some(ref mgr) = manager {
                        collect_native_fps(mgr, &current_monitors, 60)
                    } else {
                        (BTreeMap::new(), 60)
                    };
                    let (new_icon, new_ctrl) = build_initial_tray_icon(
                        &saved_wallpapers_dir,
                        &wp_state.borrow(),
                        &current_monitors,
                        &fps_map,
                        global_fps,
                    );
                    tray_icon = new_icon;
                    controls = new_ctrl;
                    log_msg("[ArcPaper] Recovery after Explorer restart complete.");
                }

                Event::UserEvent(UserEvent::TrayMenu(menu_event)) => {
                    if let Some(action) = controls.actions.get(&menu_event.id).cloned() {
                        let outcome = handle_tray_action(
                            action,
                            &mut manager,
                            workerw_hwnd,
                            &current_monitors,
                            &wp_state,
                            &current_coverage_map,
                            &saved_wallpapers_dir,
                            &config_path,
                            &tray_icon,
                            &mut controls,
                            event_host.as_mut(),
                        );
                        if outcome == TrayOutcome::Exit {
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                }

                Event::MainEventsCleared => {
                    while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
                        let _ = proxy_tray.send_event(UserEvent::TrayMenu(menu_event));
                    }
                    if let Some(ref mut mgr) = manager {
                        mgr.poll_recovery();
                    }
                    let now = std::time::Instant::now();
                    if display_debounce.poll_ready(now) {
                        handle_topology_change_if_needed(
                            &mut current_monitors,
                            &monitors_snapshot,
                            &mut manager,
                            workerw_hwnd,
                            &wp_state,
                            &current_coverage_map,
                            &saved_wallpapers_dir,
                            &tray_icon,
                            &mut controls,
                        );
                    }
                }

                _ => {}
            }
        });
    }
}

#[cfg(windows)]
fn main() {
    windows_app::run();
}
