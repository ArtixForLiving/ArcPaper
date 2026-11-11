#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod windows_app {
    use arc_paper::app::autopause_monitor::{
        apply_manual_pause_toggle, compute_autopause_transition, spawn_monitor_thread,
    };
    use arc_paper::app::autostart::{disable_autostart, enable_autostart, is_autostart_enabled};
    use arc_paper::app::config::{AppConfig, load_config, save_config};
    use arc_paper::app::log::{
        DEFAULT_MAX_LOG_BACKUPS, DEFAULT_MAX_LOG_BYTES, LogLevel, init_logger, log_msg,
        log_with_level,
    };
    use arc_paper::app::paths::{
        configure_process_and_dpi, get_app_dir, get_config_path, get_saved_wallpapers_dir,
        list_mp4_files,
    };
    use arc_paper::app::tray::{build_initial_tray_icon, rebuild_tray};
    use arc_paper::app::uninstall::perform_uninstall;
    use arc_paper::app::updater::check_for_updates;
    use arc_paper::arc_player::{ArcPlayer, AutoPause};
    use arc_paper::workerw;

    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;
    use std::time::Duration;
    use tao::event::Event;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::menu::MenuEvent;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    enum UserEvent {
        TrayMenu(MenuEvent),
        AutoPause(bool),
    }

    fn log_fps_limit(target_fps: Option<u32>) {
        match target_fps {
            Some(fps) => log_msg(&format!(
                "[ArcPaper] Applying FPS pacing limit: {} FPS",
                fps
            )),
            None => log_msg("[ArcPaper] Resetting FPS pacing to Native frame rate"),
        }
    }

    fn apply_player_settings(player: &ArcPlayer, state: &AppConfig) {
        let _ = player.set_speed(state.speed);
        let _ = player.set_brightness(state.brightness);
        let _ = player.set_fit(&state.fit_mode);
        log_fps_limit(state.target_fps);
        let _ = player.apply_fps_limit(state.target_fps);
    }

    fn activate_wallpaper(
        video_path: &Path,
        video_name: String,
        arc_player: Option<&ArcPlayer>,
        wp_state: &Rc<RefCell<AppConfig>>,
        current_native_fps: &mut u32,
        is_auto_paused: &mut bool,
        config_path: &Path,
    ) {
        if let Some(player) = arc_player {
            if let Err(e) = player.load_file(video_path) {
                log_with_level(
                    LogLevel::Error,
                    &format!("[ArcPaper] Error loading video: {}", e),
                );
            }
            if let Some(fps) = player.detect_native_fps() {
                *current_native_fps = fps.round() as u32;
            }
        }

        let mut state = wp_state.borrow_mut();
        state.active_video = Some(video_name);
        state.is_manually_paused = false;
        *is_auto_paused = false;
        if let Some(target) = state.target_fps
            && target >= *current_native_fps
        {
            state.target_fps = None;
        }
        save_config(config_path, &state);

        if let Some(player) = arc_player {
            apply_player_settings(player, &state);
            let _ = player.set_pause(false);
        }
    }

    pub fn run() {
        configure_process_and_dpi();
        init_logger(
            get_app_dir(),
            DEFAULT_MAX_LOG_BYTES,
            DEFAULT_MAX_LOG_BACKUPS,
        );

        let _instance_mutex = unsafe {
            use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
            use windows::Win32::System::Threading::CreateMutexW;
            use windows::core::w;

            let handle = CreateMutexW(None, true, w!("ArcPaper_SingleInstance_Mutex"));
            if GetLastError() == ERROR_ALREADY_EXISTS {
                log_msg(
                    "[ArcPaper] Another instance of ArcPaper is already running. Exiting cleanly.",
                );
                std::process::exit(0);
            }
            handle
        };

        std::panic::set_hook(Box::new(|info| {
            log_with_level(LogLevel::Error, &format!("[PANIC] {:?}", info));
        }));

        log_msg("[ArcPaper] Starting ArcPaper Native Wallpaper Engine");
        let (screen_w, screen_h) =
            unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        unsafe {
            use windows::Win32::System::Threading::{
                BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
            };
            let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        }

        let saved_wallpapers_dir = get_saved_wallpapers_dir();
        let config_path = get_config_path();
        let target_wid = workerw::attach_and_configure_desktop_surface(screen_w, screen_h);
        let arc_player = if !target_wid.0.is_null() {
            ArcPlayer::new(target_wid, screen_w as u32, screen_h as u32)
                .map_err(|e| {
                    log_with_level(LogLevel::Error, &format!("[ArcPaper] Init error: {e}"))
                })
                .ok()
        } else {
            log_msg("[ArcPaper] Target desktop handle null; running without active renderer.");
            None
        };

        let wp_state: Rc<RefCell<AppConfig>> = Rc::new(RefCell::new(load_config(&config_path)));
        let mut current_native_fps = 60u32;

        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        let (tray_icon, mut controls) = build_initial_tray_icon(
            &saved_wallpapers_dir,
            &wp_state.borrow(),
            current_native_fps,
        );

        let proxy_tray = event_loop.create_proxy();
        let proxy_tray_cb = proxy_tray.clone();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = proxy_tray_cb.send_event(UserEvent::TrayMenu(event));
        }));

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

        if let Some(name) = video_to_play {
            let video_path = saved_wallpapers_dir.join(&name);
            if let Some(ref player) = arc_player {
                if let Err(e) = player.load_file(&video_path) {
                    log_with_level(LogLevel::Error, &format!("[ArcPaper] load_file error: {e}"));
                }
                if let Some(fps) = player.detect_native_fps() {
                    current_native_fps = fps.round() as u32;
                }
                let state = wp_state.borrow();
                apply_player_settings(player, &state);
                if state.is_manually_paused {
                    let _ = player.set_pause(true);
                }
            }
            controls = rebuild_tray(
                &tray_icon,
                &saved_wallpapers_dir,
                &wp_state.borrow(),
                current_native_fps,
            );
        }

        let proxy_autopause = event_loop.create_proxy();
        spawn_monitor_thread(move |is_covered| {
            let _ = proxy_autopause.send_event(UserEvent::AutoPause(is_covered));
        });

        let mut is_auto_paused = false;
        let mut last_known_is_covered = false;
        event_loop.run(move |event, _, control_flow| {
            *control_flow =
                ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(200));
            let _keep_tray = &tray_icon;

            while tray_icon::TrayIconEvent::receiver().try_recv().is_ok() {}
            if let Some(ref player) = arc_player {
                player.drain_events();
            }

            match event {
                Event::UserEvent(UserEvent::AutoPause(is_covered)) => {
                    last_known_is_covered = is_covered;
                    let state = wp_state.borrow();
                    let (effective_paused, next_auto_paused, log_line) =
                        compute_autopause_transition(
                            is_covered,
                            state.is_manually_paused,
                            is_auto_paused,
                        );
                    is_auto_paused = next_auto_paused;
                    if let Some(ref player) = arc_player {
                        let _ = player.set_pause(effective_paused);
                    }
                    if let Some(msg) = log_line {
                        log_msg(msg);
                        if is_covered {
                            AutoPause::trim_working_set();
                        }
                    }
                }

                Event::UserEvent(UserEvent::TrayMenu(menu_event)) => {
                    let event_id = menu_event.id.clone();

                    if event_id == controls.upload_id {
                        if let Some(source_path) = rfd::FileDialog::new()
                            .add_filter("MP4 Video (*.mp4)", &["mp4"])
                            .set_title("Select MP4 Video File")
                            .pick_file()
                            && let Some(file_name) =
                                source_path.file_name().and_then(|n| n.to_str())
                        {
                            let dest_path = saved_wallpapers_dir.join(file_name);
                            if let Err(e) = std::fs::copy(&source_path, &dest_path) {
                                log_with_level(
                                    LogLevel::Error,
                                    &format!("[ArcPaper] Copy failed: {e:?}"),
                                );
                            } else {
                                activate_wallpaper(
                                    &dest_path,
                                    file_name.to_string(),
                                    arc_player.as_ref(),
                                    &wp_state,
                                    &mut current_native_fps,
                                    &mut is_auto_paused,
                                    &config_path,
                                );
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    current_native_fps,
                                );
                            }
                        }
                    } else if event_id == controls.pause_resume_id {
                        let mut state = wp_state.borrow_mut();
                        state.is_manually_paused = !state.is_manually_paused;
                        let (effective_paused, next_auto_paused) = apply_manual_pause_toggle(
                            last_known_is_covered,
                            state.is_manually_paused,
                        );
                        is_auto_paused = next_auto_paused;
                        if let Some(ref player) = arc_player {
                            let _ = player.set_pause(effective_paused);
                        }
                        save_config(&config_path, &state);
                        controls = rebuild_tray(
                            &tray_icon,
                            &saved_wallpapers_dir,
                            &state,
                            current_native_fps,
                        );
                    } else if let Some(&speed) = controls.speed_map.get(&event_id) {
                        if let Some(ref player) = arc_player {
                            let _ = player.set_speed(speed);
                        }
                        let mut state = wp_state.borrow_mut();
                        state.speed = speed;
                        save_config(&config_path, &state);
                    } else if let Some(&fit) = controls.fit_map.get(&event_id) {
                        if let Some(ref player) = arc_player {
                            let _ = player.set_fit(fit);
                        }
                        let mut state = wp_state.borrow_mut();
                        state.fit_mode = fit.to_string();
                        save_config(&config_path, &state);
                    } else if let Some(&brightness) = controls.brightness_map.get(&event_id) {
                        if let Some(ref player) = arc_player {
                            let _ = player.set_brightness(brightness);
                        }
                        let mut state = wp_state.borrow_mut();
                        state.brightness = brightness;
                        save_config(&config_path, &state);
                    } else if let Some(selected) = controls.wallpaper_map.get(&event_id).cloned() {
                        let full_path = saved_wallpapers_dir.join(&selected);
                        activate_wallpaper(
                            &full_path,
                            selected,
                            arc_player.as_ref(),
                            &wp_state,
                            &mut current_native_fps,
                            &mut is_auto_paused,
                            &config_path,
                        );
                        controls = rebuild_tray(
                            &tray_icon,
                            &saved_wallpapers_dir,
                            &wp_state.borrow(),
                            current_native_fps,
                        );
                    } else if let Some(&target_fps_opt) = controls.fps_map.get(&event_id) {
                        let mut state = wp_state.borrow_mut();
                        state.target_fps = target_fps_opt;
                        save_config(&config_path, &state);
                        if let Some(ref player) = arc_player {
                            log_fps_limit(target_fps_opt);
                            let _ = player.apply_fps_limit(target_fps_opt);
                        }
                        controls = rebuild_tray(
                            &tray_icon,
                            &saved_wallpapers_dir,
                            &state,
                            current_native_fps,
                        );
                    } else if event_id == controls.check_updates_id {
                        std::thread::spawn(check_for_updates);
                    } else if event_id == controls.autostart_id {
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
                        controls = rebuild_tray(
                            &tray_icon,
                            &saved_wallpapers_dir,
                            &wp_state.borrow(),
                            current_native_fps,
                        );
                    } else if event_id == controls.uninstall_id {
                        perform_uninstall();
                    } else if event_id == controls.exit_id {
                        log_msg("[ArcPaper] Exit requested. Shutting down cleanly...");
                        *control_flow = ControlFlow::Exit;
                    }
                }

                Event::MainEventsCleared => {
                    while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
                        let _ = proxy_tray.send_event(UserEvent::TrayMenu(menu_event));
                    }
                    if let Some(ref player) = arc_player {
                        player.drain_events();
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
