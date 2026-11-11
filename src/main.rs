#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod windows_app {
    use arc_paper::app::autopause_monitor::{resolve_effective_pause, spawn_multi_monitor_thread};
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
    use arc_paper::app::tray::{TrayAction, TrayControls, build_initial_tray_icon, rebuild_tray};
    use arc_paper::app::uninstall::perform_uninstall;
    use arc_paper::app::updater::check_for_updates;
    use arc_paper::arc_player::{AutoPause, WallpaperManager};
    use arc_paper::display::topology::DisplayDebounce;
    use arc_paper::display::{MonitorId, MonitorInfo, diff_topologies, enumerate_monitors};
    use arc_paper::workerw;

    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;
    use tao::event::Event;
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
    use tray_icon::menu::MenuEvent;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA;

    enum UserEvent {
        TrayMenu(MenuEvent),
        AutoPauseMap(BTreeMap<MonitorId, bool>),
        DisplayTopologyDirty,
        ExplorerRestarted,
    }

    struct EventHostContext {
        proxy: EventLoopProxy<UserEvent>,
        taskbar_created_msg: u32,
    }

    struct ArcPaperEventHost {
        hwnd: HWND,
        ctx: *mut EventHostContext,
    }

    unsafe extern "system" fn event_host_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::{
            DefWindowProcW, SetWindowLongPtrW, WM_CREATE, WM_DISPLAYCHANGE, WM_DPICHANGED,
            WM_SETTINGCHANGE,
        };

        unsafe {
            if msg == WM_CREATE {
                let create_struct =
                    &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
                let ctx_ptr = create_struct.lpCreateParams as isize;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx_ptr);
                return LRESULT(0);
            }

            let ctx_ptr =
                windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ctx_ptr != 0 {
                let ctx = &*(ctx_ptr as *const EventHostContext);

                const SPI_SETWORKAREA: usize = 0x002F;
                const SPI_SETLOGICALDPIOVERRIDE: usize = 0x009F;

                if msg == WM_DISPLAYCHANGE || msg == WM_DPICHANGED {
                    let _ = ctx.proxy.send_event(UserEvent::DisplayTopologyDirty);
                    return LRESULT(0);
                } else if msg == WM_SETTINGCHANGE {
                    if wparam.0 == SPI_SETWORKAREA || wparam.0 == SPI_SETLOGICALDPIOVERRIDE {
                        let _ = ctx.proxy.send_event(UserEvent::DisplayTopologyDirty);
                        return LRESULT(0);
                    }
                } else if ctx.taskbar_created_msg != 0 && msg == ctx.taskbar_created_msg {
                    let _ = ctx.proxy.send_event(UserEvent::ExplorerRestarted);
                    return LRESULT(0);
                }
            }

            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }

    impl ArcPaperEventHost {
        pub fn new(proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::UI::WindowsAndMessaging::{
                CS_HREDRAW, CS_VREDRAW, CreateWindowExW, HCURSOR, HICON, RegisterClassExW,
                RegisterWindowMessageW, SetWindowLongPtrW, WNDCLASSEXW, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_POPUP,
            };
            use windows::core::{PCWSTR, w};

            unsafe {
                let hinstance = match GetModuleHandleW(None) {
                    Ok(h) => h,
                    Err(e) => return Err(format!("GetModuleHandleW failed: {e}")),
                };
                let hinst = HINSTANCE(hinstance.0);
                let class_name = w!("ArcPaperEventHost");

                let wcx = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(event_host_wnd_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinst,
                    hIcon: HICON::default(),
                    hCursor: HCURSOR::default(),
                    hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
                    lpszMenuName: PCWSTR::null(),
                    lpszClassName: class_name,
                    hIconSm: HICON::default(),
                };
                let _ = RegisterClassExW(&wcx);

                let taskbar_created_msg = RegisterWindowMessageW(w!("TaskbarCreated"));

                let ctx = Box::into_raw(Box::new(EventHostContext {
                    proxy,
                    taskbar_created_msg,
                }));

                let hwnd = match CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class_name,
                    w!("ArcPaperEventHostWindow"),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(hinst),
                    Some(ctx as *const _ as *const std::ffi::c_void),
                ) {
                    Ok(h) => h,
                    Err(e) => {
                        drop(Box::from_raw(ctx));
                        return Err(format!("CreateWindowExW failed: {e}"));
                    }
                };

                SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);

                Ok(Self { hwnd, ctx })
            }
        }
    }

    impl Drop for ArcPaperEventHost {
        fn drop(&mut self) {
            unsafe {
                if !self.hwnd.0.is_null() {
                    use windows::Win32::UI::WindowsAndMessaging::{
                        DestroyWindow, SetWindowLongPtrW,
                    };
                    SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                    let _ = DestroyWindow(self.hwnd);
                    self.hwnd = HWND::default();
                }
                if !self.ctx.is_null() {
                    drop(Box::from_raw(self.ctx));
                    self.ctx = std::ptr::null_mut();
                }
            }
        }
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

    fn compute_effective_paused_map(
        monitors: &[MonitorInfo],
        config: &AppConfig,
        coverage_map: &BTreeMap<MonitorId, bool>,
    ) -> BTreeMap<MonitorId, bool> {
        let mut map = BTreeMap::new();
        for mon in monitors {
            let covered = coverage_map.get(&mon.id).copied().unwrap_or(false);
            let resolved = config.resolve_for_monitor(&mon.id);
            let effective = resolve_effective_pause(covered, resolved.is_manually_paused);
            map.insert(mon.id.clone(), effective);
        }
        map
    }

    fn collect_native_fps(
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

    #[allow(clippy::too_many_arguments)]
    fn handle_topology_change_if_needed(
        current_monitors: &mut Vec<MonitorInfo>,
        monitors_snapshot: &Arc<RwLock<Vec<MonitorInfo>>>,
        manager: &mut Option<WallpaperManager>,
        workerw_hwnd: HWND,
        wp_state: &Rc<RefCell<AppConfig>>,
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
            let effective_paused = compute_effective_paused_map(
                current_monitors,
                &wp_state.borrow(),
                current_coverage_map,
            );
            if let Some(mgr) = manager.as_mut() {
                mgr.apply_topology_and_config(
                    workerw_hwnd,
                    current_monitors,
                    &wp_state.borrow(),
                    &effective_paused,
                    saved_wallpapers_dir,
                );
            }
            let (fps_map, global_fps) = if let Some(mgr) = manager.as_ref() {
                collect_native_fps(mgr, current_monitors, 60)
            } else {
                (BTreeMap::new(), 60)
            };
            *controls = rebuild_tray(
                tray_icon,
                saved_wallpapers_dir,
                &wp_state.borrow(),
                current_monitors,
                &fps_map,
                global_fps,
            );
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
        unsafe {
            use windows::Win32::System::Threading::{
                BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
            };
            let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        }

        let saved_wallpapers_dir = get_saved_wallpapers_dir();
        let config_path = get_config_path();
        let mut workerw_hwnd = workerw::attach_virtual_desktop_surface();
        let mut current_monitors = enumerate_monitors();
        let monitors_snapshot = Arc::new(RwLock::new(current_monitors.clone()));

        let mut manager = match WallpaperManager::new(workerw_hwnd) {
            Ok(m) => Some(m),
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!("[ArcPaper] Init WallpaperManager error: {e}"),
                );
                None
            }
        };

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

        let effective_paused = compute_effective_paused_map(
            &current_monitors,
            &wp_state.borrow(),
            &current_coverage_map,
        );

        if let Some(ref mut mgr) = manager {
            mgr.apply_topology_and_config(
                workerw_hwnd,
                &current_monitors,
                &wp_state.borrow(),
                &effective_paused,
                &saved_wallpapers_dir,
            );
        }

        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

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

        let proxy_event_host = event_loop.create_proxy();
        let event_host = match ArcPaperEventHost::new(proxy_event_host) {
            Ok(host) => Some(host),
            Err(e) => {
                log_with_level(
                    LogLevel::Error,
                    &format!("[ArcPaper] Failed to initialize ArcPaperEventHost: {e}"),
                );
                None
            }
        };

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
            let _keep_event_host = &event_host;

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
                    current_coverage_map = coverage_map;
                    let effective_paused = compute_effective_paused_map(
                        &current_monitors,
                        &wp_state.borrow(),
                        &current_coverage_map,
                    );
                    if let Some(ref mut mgr) = manager {
                        mgr.update_pause_states(&wp_state.borrow(), &effective_paused);
                    }
                    let all_paused =
                        !effective_paused.is_empty() && effective_paused.values().all(|&p| p);
                    if all_paused {
                        AutoPause::trim_working_set();
                    }
                }

                Event::UserEvent(UserEvent::DisplayTopologyDirty) => {
                    display_debounce.trigger_at(std::time::Instant::now());
                }

                Event::UserEvent(UserEvent::ExplorerRestarted) => {
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
                        let effective_paused = compute_effective_paused_map(
                            &current_monitors,
                            &wp_state.borrow(),
                            &current_coverage_map,
                        );
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
                        match action {
                            TrayAction::UploadWallpaper(target) => {
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
                                        let mut state = wp_state.borrow_mut();
                                        match &target {
                                            Some(mid) => {
                                                let entry = state
                                                    .monitors
                                                    .entry(mid.as_str().to_string())
                                                    .or_default();
                                                entry.active_video = Some(file_name.to_string());
                                                entry.is_manually_paused = Some(false);
                                            }
                                            None => {
                                                state.active_video = Some(file_name.to_string());
                                                state.is_manually_paused = false;
                                            }
                                        }
                                        save_config(&config_path, &state);
                                        drop(state);

                                        let effective_paused = compute_effective_paused_map(
                                            &current_monitors,
                                            &wp_state.borrow(),
                                            &current_coverage_map,
                                        );
                                        if let Some(ref mut mgr) = manager {
                                            mgr.apply_topology_and_config(
                                                workerw_hwnd,
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
                                        controls = rebuild_tray(
                                            &tray_icon,
                                            &saved_wallpapers_dir,
                                            &wp_state.borrow(),
                                            &current_monitors,
                                            &fps_map,
                                            global_fps,
                                        );
                                    }
                                }
                            }

                            TrayAction::SelectWallpaper { monitor_id, file } => {
                                let mut state = wp_state.borrow_mut();
                                match &monitor_id {
                                    Some(mid) => {
                                        let entry = state
                                            .monitors
                                            .entry(mid.as_str().to_string())
                                            .or_default();
                                        entry.active_video = Some(file);
                                        entry.is_manually_paused = Some(false);
                                    }
                                    None => {
                                        state.active_video = Some(file);
                                        state.is_manually_paused = false;
                                    }
                                }
                                save_config(&config_path, &state);
                                drop(state);

                                let effective_paused = compute_effective_paused_map(
                                    &current_monitors,
                                    &wp_state.borrow(),
                                    &current_coverage_map,
                                );
                                if let Some(ref mut mgr) = manager {
                                    mgr.apply_topology_and_config(
                                        workerw_hwnd,
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
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    &current_monitors,
                                    &fps_map,
                                    global_fps,
                                );
                            }

                            TrayAction::SetDisplayMode(new_mode) => {
                                let mut state = wp_state.borrow_mut();
                                state.display_mode = new_mode;
                                save_config(&config_path, &state);
                                drop(state);

                                let effective_paused = compute_effective_paused_map(
                                    &current_monitors,
                                    &wp_state.borrow(),
                                    &current_coverage_map,
                                );
                                if let Some(ref mut mgr) = manager {
                                    mgr.apply_topology_and_config(
                                        workerw_hwnd,
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
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    &current_monitors,
                                    &fps_map,
                                    global_fps,
                                );
                            }

                            TrayAction::TogglePause(target) => {
                                let mut state = wp_state.borrow_mut();
                                match &target {
                                    Some(mid) => {
                                        let currently_paused =
                                            state.resolve_for_monitor(mid).is_manually_paused;
                                        let entry = state
                                            .monitors
                                            .entry(mid.as_str().to_string())
                                            .or_default();
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
                                save_config(&config_path, &state);
                                drop(state);

                                let effective_paused = compute_effective_paused_map(
                                    &current_monitors,
                                    &wp_state.borrow(),
                                    &current_coverage_map,
                                );
                                if let Some(ref mut mgr) = manager {
                                    mgr.update_pause_states(&wp_state.borrow(), &effective_paused);
                                }
                                let (fps_map, global_fps) = if let Some(ref mgr) = manager {
                                    collect_native_fps(mgr, &current_monitors, 60)
                                } else {
                                    (BTreeMap::new(), 60)
                                };
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    &current_monitors,
                                    &fps_map,
                                    global_fps,
                                );
                            }

                            TrayAction::SetFit { monitor_id, fit } => {
                                let mut state = wp_state.borrow_mut();
                                match &monitor_id {
                                    Some(mid) => {
                                        let ov = state
                                            .monitors
                                            .entry(mid.as_str().to_string())
                                            .or_default();
                                        ov.fit_mode = Some(fit.to_string());
                                    }
                                    None => {
                                        state.fit_mode = fit.to_string();
                                    }
                                }
                                save_config(&config_path, &state);
                                drop(state);

                                let effective_paused = compute_effective_paused_map(
                                    &current_monitors,
                                    &wp_state.borrow(),
                                    &current_coverage_map,
                                );
                                if let Some(ref mut mgr) = manager {
                                    mgr.apply_topology_and_config(
                                        workerw_hwnd,
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
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    &current_monitors,
                                    &fps_map,
                                    global_fps,
                                );
                            }

                            TrayAction::SetBrightness {
                                monitor_id,
                                brightness,
                            } => {
                                let mut state = wp_state.borrow_mut();
                                match &monitor_id {
                                    Some(mid) => {
                                        let ov = state
                                            .monitors
                                            .entry(mid.as_str().to_string())
                                            .or_default();
                                        ov.brightness = Some(brightness);
                                    }
                                    None => {
                                        state.brightness = brightness;
                                    }
                                }
                                save_config(&config_path, &state);
                                drop(state);

                                let effective_paused = compute_effective_paused_map(
                                    &current_monitors,
                                    &wp_state.borrow(),
                                    &current_coverage_map,
                                );
                                if let Some(ref mut mgr) = manager {
                                    mgr.apply_topology_and_config(
                                        workerw_hwnd,
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
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    &current_monitors,
                                    &fps_map,
                                    global_fps,
                                );
                            }

                            TrayAction::SetSpeed { monitor_id, speed } => {
                                let mut state = wp_state.borrow_mut();
                                match &monitor_id {
                                    Some(mid) => {
                                        let ov = state
                                            .monitors
                                            .entry(mid.as_str().to_string())
                                            .or_default();
                                        ov.speed = Some(speed);
                                    }
                                    None => {
                                        state.speed = speed;
                                    }
                                }
                                save_config(&config_path, &state);
                                drop(state);

                                let effective_paused = compute_effective_paused_map(
                                    &current_monitors,
                                    &wp_state.borrow(),
                                    &current_coverage_map,
                                );
                                if let Some(ref mut mgr) = manager {
                                    mgr.apply_topology_and_config(
                                        workerw_hwnd,
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
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    &current_monitors,
                                    &fps_map,
                                    global_fps,
                                );
                            }

                            TrayAction::SetTargetFps {
                                monitor_id,
                                target_fps,
                            } => {
                                let mut state = wp_state.borrow_mut();
                                match &monitor_id {
                                    Some(mid) => {
                                        let ov = state
                                            .monitors
                                            .entry(mid.as_str().to_string())
                                            .or_default();
                                        ov.target_fps = target_fps;
                                    }
                                    None => {
                                        state.target_fps = target_fps;
                                    }
                                }
                                log_fps_limit(target_fps);
                                save_config(&config_path, &state);
                                drop(state);

                                let effective_paused = compute_effective_paused_map(
                                    &current_monitors,
                                    &wp_state.borrow(),
                                    &current_coverage_map,
                                );
                                if let Some(ref mut mgr) = manager {
                                    mgr.apply_topology_and_config(
                                        workerw_hwnd,
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
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    &current_monitors,
                                    &fps_map,
                                    global_fps,
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
                                let (fps_map, global_fps) = if let Some(ref mgr) = manager {
                                    collect_native_fps(mgr, &current_monitors, 60)
                                } else {
                                    (BTreeMap::new(), 60)
                                };
                                controls = rebuild_tray(
                                    &tray_icon,
                                    &saved_wallpapers_dir,
                                    &wp_state.borrow(),
                                    &current_monitors,
                                    &fps_map,
                                    global_fps,
                                );
                            }

                            TrayAction::Uninstall => {
                                perform_uninstall();
                            }

                            TrayAction::Exit => {
                                log_msg("[ArcPaper] Exit requested. Shutting down cleanly...");
                                *control_flow = ControlFlow::Exit;
                            }
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
