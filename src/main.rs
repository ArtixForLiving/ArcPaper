use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use serde::{Deserialize, Serialize};
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Window, WindowBuilder, WindowId};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIconBuilder};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetClassNameW, GetClientRect, GetForegroundWindow, GetSystemMetrics,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed,
    SendMessageTimeoutW, SetParent, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE,
    GWL_STYLE, HWND_BOTTOM, HWND_TOP, SMTO_NORMAL, SM_CXSCREEN, SM_CYSCREEN, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_DLGFRAME, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_STATICEDGE,
    WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
    WS_VISIBLE,
};
use wry::http::{header, Response, StatusCode};
use wry::WebViewBuilder;

// ─── Logging ──────────────────────────────────────────────────────────────────

fn log_msg(msg: &str) {
    println!("{}", msg);
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("D:\\Arc Paper\\app.log")
    {
        let _ = writeln!(file, "{}", msg);
    }
}

// ─── WorkerW Integration ───────────────────────────────────────────────────

/// Attaches the window behind Windows 11 desktop icons (24H2 / 25H2).
/// Attaches the window behind Windows 11 desktop icons (24H2 / 25H2).
fn attach_window_behind_desktop_icons(my_hwnd: HWND, width: i32, height: i32) {
    unsafe {
        // 1. Locate "Progman" window handle via FindWindowW
        let progman = match FindWindowW(w!("Progman"), PCWSTR::null()) {
            Ok(h) if !h.0.is_null() => h,
            _ => {
                log_msg("[ArcPaper] Progman window not found in current session.");
                return;
            }
        };
        log_msg(&format!("[ArcPaper] Found Progman HWND: {:?}", progman));

        // Send 0x052C message to Progman (wParam 0xD, lParam 0x1) to spawn WorkerW
        let mut result = 0usize;
        let sm_res = SendMessageTimeoutW(
            progman,
            0x052C,
            WPARAM(0xD),
            LPARAM(0x1),
            SMTO_NORMAL,
            1000,
            Some(&mut result),
        );
        log_msg(&format!(
            "[ArcPaper] SendMessageTimeoutW 0x052C result: {:?}, result_val: {}",
            sm_res, result
        ));

        // 2. Windows 11 25H2: Locate "SHELLDLL_DefView" inside Progman using FindWindowExW
        let mut shelldll_defview =
            match FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), PCWSTR::null()) {
                Ok(h) if !h.0.is_null() => h,
                _ => HWND::default(),
            };

        // Fallback search for SHELLDLL_DefView if Explorer hosts it in a top-level WorkerW
        if shelldll_defview.0.is_null() {
            let mut top_worker =
                FindWindowExW(None, None, w!("WorkerW"), PCWSTR::null()).unwrap_or(HWND::default());
            while !top_worker.0.is_null() {
                let child_shell = FindWindowExW(
                    Some(top_worker),
                    None,
                    w!("SHELLDLL_DefView"),
                    PCWSTR::null(),
                )
                .unwrap_or(HWND::default());
                if !child_shell.0.is_null() {
                    shelldll_defview = child_shell;
                    break;
                }
                top_worker = FindWindowExW(None, Some(top_worker), w!("WorkerW"), PCWSTR::null())
                    .unwrap_or(HWND::default());
            }
        }
        log_msg(&format!("[ArcPaper] SHELLDLL_DefView HWND: {:?}", shelldll_defview));

        // 3. Locate "WorkerW" inside Progman using FindWindowExW
        let workerw =
            match FindWindowExW(Some(progman), None, w!("WorkerW"), PCWSTR::null()) {
                Ok(h) if !h.0.is_null() => h,
                _ => HWND::default(),
            };
        log_msg(&format!("[ArcPaper] WorkerW in Progman HWND: {:?}", workerw));

        // Determine target parent: WorkerW if available, otherwise Progman
        let target_parent = if !workerw.0.is_null() {
            workerw
        } else {
            progman
        };
        log_msg(&format!("[ArcPaper] Target Parent HWND: {:?}", target_parent));

        // 4. Strip invisible resize borders (WS_THICKFRAME, WS_CAPTION, etc.) before SetParent
        let cur_style = GetWindowLongPtrW(my_hwnd, GWL_STYLE) as u32;
        let clean_style = (cur_style
            & !(WS_CAPTION.0
                | WS_THICKFRAME.0
                | WS_BORDER.0
                | WS_DLGFRAME.0
                | WS_SYSMENU.0
                | WS_POPUP.0
                | WS_MINIMIZEBOX.0
                | WS_MAXIMIZEBOX.0))
            | WS_CHILD.0
            | WS_VISIBLE.0
            | WS_CLIPSIBLINGS.0
            | WS_CLIPCHILDREN.0;
        let _ = SetWindowLongPtrW(my_hwnd, GWL_STYLE, clean_style as isize);

        let cur_ex = GetWindowLongPtrW(my_hwnd, GWL_EXSTYLE) as u32;
        let clean_ex = cur_ex
            & !(WS_EX_WINDOWEDGE.0
                | WS_EX_CLIENTEDGE.0
                | WS_EX_STATICEDGE.0
                | WS_EX_DLGMODALFRAME.0);
        let _ = SetWindowLongPtrW(my_hwnd, GWL_EXSTYLE, clean_ex as isize);

        // Attach window to target parent (WorkerW or Progman) via SetParent
        let prev_parent = SetParent(my_hwnd, Some(target_parent));
        log_msg(&format!("[ArcPaper] SetParent result (prev parent): {:?}", prev_parent));

        // Re-enforce clean style after SetParent
        let _ = SetWindowLongPtrW(my_hwnd, GWL_STYLE, clean_style as isize);
        let _ = SetWindowLongPtrW(my_hwnd, GWL_EXSTYLE, clean_ex as isize);

        // Ensure target_parent has WS_CLIPCHILDREN to prevent painting over child window
        let p_style = GetWindowLongPtrW(target_parent, GWL_STYLE) as u32;
        let _ = SetWindowLongPtrW(target_parent, GWL_STYLE, (p_style | WS_CLIPCHILDREN.0) as isize);

        // Initial position window at x: 0, y: 0, cx: width, cy: height behind SHELLDLL_DefView
        if !shelldll_defview.0.is_null() {
            if target_parent == workerw {
                let _ = SetWindowPos(
                    workerw,
                    Some(shelldll_defview),
                    0,
                    0,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
                let _ = SetWindowPos(
                    my_hwnd,
                    Some(HWND_TOP),
                    0,
                    0,
                    width,
                    height,
                    SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            } else {
                let _ = SetWindowPos(
                    my_hwnd,
                    Some(shelldll_defview),
                    0,
                    0,
                    width,
                    height,
                    SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        } else {
            let _ = SetWindowPos(
                my_hwnd,
                Some(HWND_BOTTOM),
                0,
                0,
                width,
                height,
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }

        // Measure resulting client area
        let mut client_rect = RECT::default();
        let _ = GetClientRect(my_hwnd, &mut client_rect);
        let c_width = client_rect.right - client_rect.left;
        let c_height = client_rect.bottom - client_rect.top;

        log_msg(&format!(
            "[ArcPaper] Client area after initial SetWindowPos: {}x{} (target: {}x{})",
            c_width, c_height, width, height
        ));

        // If non-client borders are still taking up pixels on left/right/bottom, compensate:
        let dx = (width - c_width) / 2;
        let dy = height - c_height;
        if dx > 0 || dy > 0 {
            log_msg(&format!(
                "[ArcPaper] Compensating for non-client borders: dx={}, dy={}",
                dx, dy
            ));
            if target_parent == workerw {
                let _ = SetWindowPos(
                    workerw,
                    Some(shelldll_defview),
                    -dx,
                    0,
                    width + (dx * 2),
                    height + dy,
                    SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            let _ = SetWindowPos(
                my_hwnd,
                Some(HWND_TOP),
                -dx,
                0,
                width + (dx * 2),
                height + dy,
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }

        let _ = UpdateWindow(my_hwnd);
        let _ = ShowWindow(my_hwnd, SW_SHOW);
        log_msg("[ArcPaper] Window successfully attached behind desktop icons with 100% gapless coverage.");
    }
}

// ─── Tray Icon ────────────────────────────────────────────────────────────────

/// Creates a simple 32x32 RGBA icon for the system tray
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

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Decodes percent-encoded URL paths
fn percent_decode(input: &str) -> String {
    let mut result = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte_val) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                result.push(byte_val);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

/// Resolves the application root directory
fn get_app_dir() -> PathBuf {
    let fixed_dir = PathBuf::from("D:\\Arc Paper");
    if fixed_dir.exists() {
        return fixed_dir;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Resolves a valid SavedWallpapers directory
fn get_saved_wallpapers_dir() -> PathBuf {
    let fixed_dir = PathBuf::from("D:\\Arc Paper\\SavedWallpapers");
    if fixed_dir.exists() {
        return fixed_dir;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p_exe = parent.join("SavedWallpapers");
            if p_exe.exists() {
                return p_exe;
            }
        }
    }
    let p_cur = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("SavedWallpapers");
    let _ = std::fs::create_dir_all(&p_cur);
    p_cur
}

/// Resolves the config.json file path
fn get_config_path() -> PathBuf {
    get_app_dir().join("config.json")
}

/// Returns sorted list of .mp4 filenames in the SavedWallpapers directory
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

/// Plays a video in the wallpaper WebView via evaluate_script
fn play_wallpaper(webview: &wry::WebView, filename: &str) {
    let escaped = filename
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    let script = format!("playWallpaper('{}');", escaped);
    if let Err(e) = webview.evaluate_script(&script) {
        log_msg(&format!("[ArcPaper] Failed to execute evaluate_script: {:?}", e));
    } else {
        log_msg(&format!("[ArcPaper] evaluate_script dispatched for: {}", filename));
    }
}

/// Escapes a string for safe embedding inside a JS string literal (single-quoted)
fn js_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

// ─── Configuration (config.json) ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    pub active_video: Option<String>,
    pub speed: f32,
    pub brightness: u32,
    pub fit_mode: String,
    pub is_manually_paused: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            active_video: None,
            speed: 1.0,
            brightness: 100,
            fit_mode: "cover".to_string(),
            is_manually_paused: false,
        }
    }
}

fn load_config() -> AppConfig {
    let path = get_config_path();
    if path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<AppConfig>(&content) {
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
            log_msg(&format!("[ArcPaper] Error saving config to {:?}: {:?}", path, e));
        } else {
            log_msg(&format!("[ArcPaper] Successfully saved config to {:?}", path));
        }
    }
}

// ─── System Tray Menu ─────────────────────────────────────────────────────────

struct TrayControls {
    open_dashboard_id: MenuId,
    upload_id: MenuId,
    pause_resume_id: MenuId,
    speed_map: HashMap<MenuId, f32>,
    fit_map: HashMap<MenuId, &'static str>,
    brightness_map: HashMap<MenuId, u32>,
    wallpaper_map: HashMap<MenuId, String>,
    exit_id: MenuId,
}

/// Builds the complete system tray menu and playback controls (100% English)
fn build_tray_menu(saved_wallpapers_dir: &Path, is_paused: bool) -> (Menu, TrayControls) {
    let menu = Menu::new();

    // 0. Open Dashboard (top item)
    let open_dashboard_item = MenuItem::new("Open Dashboard", true, None);
    let open_dashboard_id = open_dashboard_item.id().clone();
    let _ = menu.append(&open_dashboard_item);

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 1. Upload & Apply Wallpaper
    let upload_item = MenuItem::new("Upload New Wallpaper...", true, None);
    let upload_id = upload_item.id().clone();
    let apply_submenu = Submenu::new("Select Active Wallpaper", true);

    let mut wallpaper_map = HashMap::new();
    let mp4_files = list_mp4_files(saved_wallpapers_dir);

    if mp4_files.is_empty() {
        let empty_item = MenuItem::new("No saved wallpapers", false, None);
        let _ = apply_submenu.append(&empty_item);
    } else {
        for filename in mp4_files {
            let item = MenuItem::new(&filename, true, None);
            wallpaper_map.insert(item.id().clone(), filename);
            let _ = apply_submenu.append(&item);
        }
    }

    let _ = menu.append(&upload_item);
    let _ = menu.append(&apply_submenu);

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 2. Play / Pause Control
    let pause_text = if is_paused {
        "Resume Wallpaper"
    } else {
        "Pause Wallpaper"
    };
    let pause_resume_item = MenuItem::new(pause_text, true, None);
    let pause_resume_id = pause_resume_item.id().clone();
    let _ = menu.append(&pause_resume_item);

    // 3. Playback Speed Sub-menu
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
    let _ = menu.append(&speed_submenu);

    // 4. Screen Fit Sub-menu
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
    let _ = menu.append(&fit_submenu);

    // 5. Brightness Sub-menu
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
    let _ = menu.append(&brightness_submenu);

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 6. Exit
    let exit_item = MenuItem::new("Exit ArcPaper", true, None);
    let exit_id = exit_item.id().clone();
    let _ = menu.append(&exit_item);

    let controls = TrayControls {
        open_dashboard_id,
        upload_id,
        pause_resume_id,
        speed_map,
        fit_map,
        brightness_map,
        wallpaper_map,
        exit_id,
    };

    (menu, controls)
}

// ─── UserEvent ────────────────────────────────────────────────────────────────

enum UserEvent {
    TrayMenu(MenuEvent),
    IpcMessage(String),
    AutoPause(bool),
}

// ─── Dashboard HTML ───────────────────────────────────────────────────────────

const DASHBOARD_HTML: &str = include_str!("dashboard.html");

// ─── Custom Protocol Handler (shared factory) ─────────────────────────────────

fn build_custom_protocol_handler(
    protocol_dir: PathBuf,
) -> impl Fn(wry::WebViewId, wry::http::Request<Vec<u8>>) -> wry::http::Response<Cow<'static, [u8]>>
       + Send
       + Sync
       + 'static {
    move |_id, request| {
        let path_part = request.uri().path();
        let decoded = percent_decode(path_part.trim_start_matches('/'));
        let file_name = Path::new(&decoded)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        let mut file_path = protocol_dir.join(file_name);
        if !file_path.is_file() {
            let alt1 = PathBuf::from("D:\\Arc Paper\\SavedWallpapers").join(file_name);
            let alt2 = PathBuf::from("D:\\Arc Paper\\target\\debug\\SavedWallpapers").join(file_name);
            if alt1.is_file() {
                file_path = alt1;
            } else if alt2.is_file() {
                file_path = alt2;
            }
        }

        if file_path.is_file() {
            match std::fs::read(&file_path) {
                Ok(file_bytes) => {
                    let total_len = file_bytes.len();
                    let range_header = request.headers().get("range").and_then(|v| v.to_str().ok());

                    let (status, body_data, content_range) = if let Some(range_str) = range_header {
                        if let Some(spec) = range_str.strip_prefix("bytes=") {
                            let parts: Vec<&str> = spec.split('-').collect();
                            let start: usize = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                            let end: usize = parts
                                .get(1)
                                .and_then(|s| if s.is_empty() { None } else { s.parse().ok() })
                                .unwrap_or(total_len.saturating_sub(1));

                            let end = end.min(total_len.saturating_sub(1));
                            if start <= end && start < total_len {
                                let slice = file_bytes[start..=end].to_vec();
                                (
                                    StatusCode::PARTIAL_CONTENT,
                                    slice,
                                    Some(format!("bytes {}-{}/{}", start, end, total_len)),
                                )
                            } else {
                                (StatusCode::OK, file_bytes, None)
                            }
                        } else {
                            (StatusCode::OK, file_bytes, None)
                        }
                    } else {
                        (StatusCode::OK, file_bytes, None)
                    };

                    let mut res_builder = Response::builder()
                        .status(status)
                        .header(header::CONTENT_TYPE, "video/mp4")
                        .header(header::ACCEPT_RANGES, "bytes")
                        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                        .header(header::CONTENT_LENGTH, body_data.len().to_string());

                    if let Some(cr) = content_range {
                        res_builder = res_builder.header(header::CONTENT_RANGE, cr);
                    }

                    res_builder.body(Cow::from(body_data)).unwrap_or_else(|_| {
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Cow::default())
                            .unwrap()
                    })
                }
                Err(e) => {
                    log_msg(&format!("[ArcPaper] Failed to read video {:?}: {:?}", file_path, e));
                    Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Cow::default())
                        .unwrap()
                }
            }
        } else {
            log_msg(&format!("[ArcPaper] Video file not found: {:?}", file_path));
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Cow::default())
                .unwrap()
        }
    }
}

// ─── Smart Auto-Pause Helper ──────────────────────────────────────────────────

/// Checks whether the foreground active window covers the screen (fullscreen or maximized)
/// Returns false if the foreground window is desktop/shell or ArcPaper's own window
unsafe fn is_foreground_covering_screen(screen_w: i32, screen_h: i32, our_pid: u32) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        if !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
            return false;
        }

        // Check if foreground belongs to ArcPaper process
        let mut fg_pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        if fg_pid == our_pid {
            return false;
        }

        // Check if window is desktop/shell
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

        // Check if maximized (covers workspace)
        if IsZoomed(fg).as_bool() {
            return true;
        }

        // Check if fullscreen (covers full screen resolution)
        let mut rect = RECT::default();
        if GetWindowRect(fg, &mut rect).is_ok() {
            if rect.left <= 0 && rect.top <= 0 && rect.right >= screen_w && rect.bottom >= screen_h {
                return true;
            }
        }

        false
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("D:\\Arc Paper\\app.log")
        {
            let _ = writeln!(f, "[PANIC] {:?}", info);
        }
    }));

    // 0. Ensure SavedWallpapers folder exists
    let saved_wallpapers_dir = get_saved_wallpapers_dir();
    let _ = std::fs::create_dir_all(&saved_wallpapers_dir);
    log_msg(&format!(
        "[ArcPaper] SavedWallpapers folder ready at: {:?}",
        saved_wallpapers_dir
    ));

    // Load persisted configuration
    let initial_config = load_config();
    let wp_state: Rc<RefCell<AppConfig>> = Rc::new(RefCell::new(initial_config));

    // 1. Initialize Tao EventLoop
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    // Detect primary display monitor
    let monitor = event_loop
        .primary_monitor()
        .or_else(|| event_loop.available_monitors().next());

    let (monitor_pos, monitor_size) = if let Some(ref m) = monitor {
        (m.position(), m.size())
    } else {
        (PhysicalPosition::new(0, 0), PhysicalSize::new(1920u32, 1080u32))
    };

    let monitor_scale = monitor.as_ref().map(|m| m.scale_factor()).unwrap_or(1.0);

    log_msg(&format!(
        "[ArcPaper] Primary monitor detected: position {:?}, size {:?}, scale {}",
        monitor_pos, monitor_size, monitor_scale
    ));

    // 2. Build wallpaper window (behind desktop icons)
    log_msg("[ArcPaper] Building wallpaper window...");
    let wallpaper_window = WindowBuilder::new()
        .with_title("ArcPaper")
        .with_decorations(false)
        .with_resizable(false)
        .with_undecorated_shadow(false)
        .with_position(PhysicalPosition::new(0, 0))
        .with_inner_size(monitor_size)
        .with_skip_taskbar(true)
        .build(&event_loop)
        .expect("Failed to create wallpaper window");

    let my_hwnd = HWND(wallpaper_window.hwnd() as *mut _);
    log_msg(&format!("[ArcPaper] Wallpaper window HWND: {:?}", my_hwnd));

    unsafe {
        let style = GetWindowLongPtrW(my_hwnd, GWL_STYLE) as u32;
        let exstyle = GetWindowLongPtrW(my_hwnd, GWL_EXSTYLE) as u32;
        let mut wr = RECT::default();
        let mut cr = RECT::default();
        let _ = GetWindowRect(my_hwnd, &mut wr);
        let _ = GetClientRect(my_hwnd, &mut cr);
        log_msg(&format!(
            "[ArcPaper DIAG] Initial wallpaper_window: style=0x{:08X}, exstyle=0x{:08X}, WindowRect={:?} ({}x{}), ClientRect={:?} ({}x{})",
            style, exstyle, wr, wr.right - wr.left, wr.bottom - wr.top, cr, cr.right - cr.left, cr.bottom - cr.top
        ));
    }

    // 3. Wallpaper HTML
    let wallpaper_html = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>ArcPaper</title>
    <style>
        * {
            margin: 0 !important;
            padding: 0 !important;
            box-sizing: border-box !important;
            user-select: none !important;
        }
        html, body {
            width: 100% !important;
            height: 100% !important;
            margin: 0 !important;
            padding: 0 !important;
            overflow: hidden !important;
            background: #000 !important;
        }
        #bg-video, video {
            position: fixed !important;
            top: 0 !important;
            left: 0 !important;
            right: 0 !important;
            bottom: 0 !important;
            width: 100% !important;
            height: 100% !important;
            min-width: 100% !important;
            min-height: 100% !important;
            display: block !important;
            border: none !important;
            outline: none !important;
            object-fit: cover;
            background: #000 !important;
        }
        #placeholder {
            position: absolute;
            top: 50%;
            left: 50%;
            transform: translate(-50%, -50%);
            text-align: center;
            color: #64ffda;
            font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
            pointer-events: none;
            transition: opacity 0.5s ease;
        }
        h1 { font-size: 3rem; font-weight: 300; letter-spacing: 2px; margin-bottom: 0.5rem; }
        p { color: #8892b0; font-size: 1.1rem; }
    </style>
</head>
<body>
    <div id="placeholder">
        <h1>ArcPaper</h1>
        <p>Right-click the system tray icon to select or upload video wallpapers (.mp4)</p>
    </div>
    <video id="bg-video" autoplay loop muted playsinline></video>
    <script>
        const video = document.getElementById('bg-video');
        const placeholder = document.getElementById('placeholder');

        // Ensure audio is permanently muted
        video.muted = true;
        video.volume = 0;

        video.addEventListener('volumechange', () => {
            if (!video.muted || video.volume > 0) {
                video.muted = true;
                video.volume = 0;
            }
        });

        video.addEventListener('playing', () => {
            console.log('[HTML Video] Video is playing');
            if (placeholder) placeholder.style.display = 'none';
        });

        video.addEventListener('error', (e) => {
            console.error('[HTML Video] Video error event:', video.error);
        });

        function playWallpaper(filename) {
            const encoded = encodeURIComponent(filename);
            const streamUrl = 'http://wallpaper.localhost/' + encoded;
            console.log('[HTML Video] Loading video source:', streamUrl);

            video.muted = true;
            video.volume = 0;
            video.src = streamUrl;
            video.style.display = 'block';
            video.load();
            video.play().then(() => {
                console.log('[HTML Video] play() promise resolved');
                if (placeholder) placeholder.style.display = 'none';
            }).catch(err => {
                console.warn('[HTML Video] play() failed, trying fallback:', err);
                video.src = 'wallpaper://localhost/' + encoded;
                video.load();
                video.play().catch(e2 => console.error('[HTML Video] Fallback failed:', e2));
            });
        }

        function togglePlayPause() {
            if (video.paused) {
                video.play().catch(err => console.error('[HTML Video] play() error:', err));
            } else {
                video.pause();
            }
        }

        function setSpeed(rate) {
            video.playbackRate = rate;
            console.log('[HTML Video] Playback rate changed to:', rate);
        }

        function setFit(fitMode) {
            video.style.objectFit = fitMode;
            console.log('[HTML Video] Object fit changed to:', fitMode);
        }

        function setBrightness(val) {
            video.style.filter = 'brightness(' + val + ')';
            console.log('[HTML Video] Brightness filter changed to:', val);
        }
    </script>
</body>
</html>"#;

    // 4. Build wallpaper WebView (custom protocol — DO NOT MODIFY)
    let protocol_dir_wallpaper = saved_wallpapers_dir.clone();
    let wallpaper_webview = WebViewBuilder::new()
        .with_background_color((0, 0, 0, 255))
        .with_html(wallpaper_html)
        .with_custom_protocol("wallpaper".into(), build_custom_protocol_handler(protocol_dir_wallpaper))
        .build(&wallpaper_window)
        .expect("Failed to initialize wallpaper WebView");

    // 5. Attach wallpaper window behind desktop icons
    attach_window_behind_desktop_icons(
        my_hwnd,
        monitor_size.width as i32,
        monitor_size.height as i32,
    );

    // Set WebView bounds to exact physical monitor size to eliminate any non-client padding
    let _ = wallpaper_webview.set_bounds(wry::Rect {
        position: wry::dpi::Position::Physical(PhysicalPosition::new(0, 0)),
        size: wry::dpi::Size::Physical(monitor_size),
    });

    // 6. System Tray
    let (initial_menu, mut controls) =
        build_tray_menu(&saved_wallpapers_dir, wp_state.borrow().is_manually_paused);
    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(initial_menu))
        .with_tooltip("ArcPaper")
        .with_icon(create_default_tray_icon())
        .build()
        .expect("Failed to create system tray icon");

    // Route tray menu events to event loop
    let proxy_tray = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy_tray.send_event(UserEvent::TrayMenu(event));
    }));

    // 7. Auto-play saved wallpaper on startup (or fallback to first file)
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
        files.into_iter().next().map(|first| {
            wp_state.borrow_mut().active_video = Some(first.clone());
            save_config(&wp_state.borrow());
            first
        })
    });

    if let Some(name) = video_to_play {
        log_msg(&format!("[ArcPaper] Playing wallpaper on startup: {}", name));
        play_wallpaper(&wallpaper_webview, &name);
        let state = wp_state.borrow();
        let _ = wallpaper_webview.evaluate_script(&format!("setSpeed({});", state.speed));
        let _ = wallpaper_webview.evaluate_script(&format!("setBrightness({});", state.brightness as f32 / 100.0));
        let _ = wallpaper_webview.evaluate_script(&format!("setFit('{}');", js_escape(&state.fit_mode)));
        if state.is_manually_paused {
            let _ = wallpaper_webview.evaluate_script("video.pause();");
        }
    }

    // Dashboard window + webview (lazily created)
    let dashboard_window: Rc<RefCell<Option<Window>>> = Rc::new(RefCell::new(None));
    let dashboard_webview: Rc<RefCell<Option<wry::WebView>>> = Rc::new(RefCell::new(None));
    let dashboard_window_id: Rc<RefCell<Option<WindowId>>> = Rc::new(RefCell::new(None));

    let proxy_ipc = event_loop.create_proxy();

    // 8. Start Smart Auto-Pause background monitor thread
    let proxy_autopause = event_loop.create_proxy();
    let our_pid = std::process::id();
    std::thread::spawn(move || {
        let mut last_state = false;
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            unsafe {
                let screen_w = GetSystemMetrics(SM_CXSCREEN);
                let screen_h = GetSystemMetrics(SM_CYSCREEN);
                let is_covered = is_foreground_covering_screen(screen_w, screen_h, our_pid);
                if is_covered != last_state {
                    last_state = is_covered;
                    let _ = proxy_autopause.send_event(UserEvent::AutoPause(is_covered));
                }
            }
        }
    });

    log_msg("[ArcPaper] Application initialized. Event loop started.");

    // ── Helper: send current state + file list to dashboard WebView ──────────
    let build_update_gallery_script = |dir: &Path, config: &AppConfig| -> String {
        let files = list_mp4_files(dir);
        let files_json: Vec<String> = files.iter().map(|f| format!("\"{}\"", js_escape(f))).collect();
        let active_json = match &config.active_video {
            Some(f) => format!("\"{}\"", js_escape(f)),
            None => "null".to_string(),
        };
        let fit_json = format!("\"{}\"", js_escape(&config.fit_mode));
        format!(
            "updateGallery([{}], {}, {}, {}, {}, {});",
            files_json.join(","),
            active_json,
            config.speed,
            config.brightness,
            fit_json,
            config.is_manually_paused
        )
    };

    // ── Helper: open or show dashboard window ────────────────────────────────
    let open_dashboard = {
        let dashboard_window = Rc::clone(&dashboard_window);
        let dashboard_webview = Rc::clone(&dashboard_webview);
        let dashboard_window_id = Rc::clone(&dashboard_window_id);
        let proxy_ipc = proxy_ipc.clone();
        let saved_wallpapers_dir = saved_wallpapers_dir.clone();
        let protocol_dir_dash = saved_wallpapers_dir.clone();

        move |event_loop_target: &tao::event_loop::EventLoopWindowTarget<UserEvent>,
              state: &AppConfig| {
            let mut dw = dashboard_window.borrow_mut();
            let mut dwv = dashboard_webview.borrow_mut();

            if dw.is_none() {
                // Calculate centered position on primary monitor using logical size & monitor scale
                let dash_w = 960.0f64;
                let dash_h = 620.0f64;
                let phys_w = (dash_w * monitor_scale).round() as i32;
                let phys_h = (dash_h * monitor_scale).round() as i32;
                let cx = monitor_pos.x + (monitor_size.width as i32 - phys_w) / 2;
                let cy = monitor_pos.y + (monitor_size.height as i32 - phys_h) / 2;

                let win = WindowBuilder::new()
                    .with_title("ArcPaper")
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_inner_size(LogicalSize::new(dash_w, dash_h))
                    .with_position(PhysicalPosition::new(cx, cy))
                    .with_skip_taskbar(false)
                    .build(event_loop_target)
                    .expect("Failed to create dashboard window");

                *dashboard_window_id.borrow_mut() = Some(win.id());

                let proxy_for_ipc = proxy_ipc.clone();
                let wv = WebViewBuilder::new()
                    .with_html(DASHBOARD_HTML)
                    .with_custom_protocol("wallpaper".into(), build_custom_protocol_handler(protocol_dir_dash.clone()))
                    .with_ipc_handler(move |msg| {
                        let _ = proxy_for_ipc.send_event(UserEvent::IpcMessage(msg.body().to_string()));
                    })
                    .build(&win)
                    .expect("Failed to build dashboard WebView");

                *dw = Some(win);
                *dwv = Some(wv);
            } else {
                // Already created — just show it
                if let Some(w) = dw.as_ref() {
                    w.set_visible(true);
                    w.set_focus();
                }
            }

            // Push current state to dashboard
            if let Some(wv) = dwv.as_ref() {
                let script = build_update_gallery_script(&saved_wallpapers_dir, state);
                let _ = wv.evaluate_script(&script);
            }
        }
    };

    // Tracking state for smart auto-pause
    let mut is_auto_paused = false;

    // ── Event Loop ───────────────────────────────────────────────────────────
    event_loop.run(move |event, event_loop_target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + Duration::from_millis(200),
        );

        let _keep_tray = &tray_icon;
        let _keep_wallpaper_window = &wallpaper_window;
        let _keep_wallpaper_webview = &wallpaper_webview;

        // Poll tray icon events (left-click → open dashboard)
        while let Ok(tray_event) = tray_icon::TrayIconEvent::receiver().try_recv() {
            if let tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = tray_event
            {
                log_msg("[ArcPaper] Tray left-click: opening Dashboard.");
                open_dashboard(event_loop_target, &wp_state.borrow());
            }
        }

        match event {
            // ── Smart Auto-Pause Event ───────────────────────────────────────
            Event::UserEvent(UserEvent::AutoPause(is_covered)) => {
                let state = wp_state.borrow();
                if is_covered {
                    if !state.is_manually_paused && !is_auto_paused {
                        is_auto_paused = true;
                        log_msg("[ArcPaper] Smart Auto-Pause: Fullscreen / maximized app detected. Pausing wallpaper.");
                        let _ = wallpaper_webview.evaluate_script("if (!video.paused) video.pause();");
                    }
                } else {
                    if !state.is_manually_paused && is_auto_paused {
                        is_auto_paused = false;
                        log_msg("[ArcPaper] Smart Auto-Resume: Desktop is visible. Resuming wallpaper.");
                        let _ = wallpaper_webview.evaluate_script("if (video.paused) video.play();");
                    }
                }
            }

            // ── Tray menu events ────────────────────────────────────────────
            Event::UserEvent(UserEvent::TrayMenu(menu_event)) => {
                let event_id = menu_event.id.clone();

                if event_id == controls.open_dashboard_id {
                    log_msg("[ArcPaper] 'Open Dashboard' tray item clicked.");
                    open_dashboard(event_loop_target, &wp_state.borrow());

                } else if event_id == controls.upload_id {
                    log_msg("[ArcPaper] Upload Wallpaper clicked. Opening file picker...");
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
                                play_wallpaper(&wallpaper_webview, file_name);

                                let mut state = wp_state.borrow_mut();
                                state.active_video = Some(file_name.to_string());
                                state.is_manually_paused = false;
                                is_auto_paused = false;
                                save_config(&state);
                                let is_paused = state.is_manually_paused;
                                drop(state);

                                // Rebuild tray menu
                                let (new_menu, new_controls) = build_tray_menu(&saved_wallpapers_dir, is_paused);
                                let _ = tray_icon.set_menu(Some(Box::new(new_menu)));
                                controls = new_controls;

                                // Refresh dashboard if open
                                let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                                if let Some(wv) = dashboard_webview.borrow().as_ref() {
                                    let _ = wv.evaluate_script(&script);
                                }
                            }
                        }
                    }

                } else if event_id == controls.exit_id {
                    log_msg("[ArcPaper] Exit clicked. Closing application gracefully...");
                    *control_flow = ControlFlow::Exit;

                } else if event_id == controls.pause_resume_id {
                    log_msg("[ArcPaper] Pause / Resume clicked via tray.");
                    let mut state = wp_state.borrow_mut();
                    state.is_manually_paused = !state.is_manually_paused;
                    if state.is_manually_paused {
                        is_auto_paused = false;
                        let _ = wallpaper_webview.evaluate_script("video.pause();");
                    } else {
                        is_auto_paused = false;
                        let _ = wallpaper_webview.evaluate_script("video.play();");
                    }
                    save_config(&state);
                    let is_paused = state.is_manually_paused;
                    drop(state);

                    // Rebuild tray menu
                    let (new_menu, new_controls) = build_tray_menu(&saved_wallpapers_dir, is_paused);
                    let _ = tray_icon.set_menu(Some(Box::new(new_menu)));
                    controls = new_controls;

                    // Refresh dashboard if open
                    let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                    if let Some(wv) = dashboard_webview.borrow().as_ref() {
                        let _ = wv.evaluate_script(&script);
                    }

                } else if let Some(&speed) = controls.speed_map.get(&event_id) {
                    log_msg(&format!("[ArcPaper] Setting playback rate: {}x", speed));
                    let _ = wallpaper_webview.evaluate_script(&format!("setSpeed({});", speed));
                    let mut state = wp_state.borrow_mut();
                    state.speed = speed;
                    save_config(&state);
                    drop(state);

                    let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                    if let Some(wv) = dashboard_webview.borrow().as_ref() {
                        let _ = wv.evaluate_script(&script);
                    }

                } else if let Some(&fit) = controls.fit_map.get(&event_id) {
                    log_msg(&format!("[ArcPaper] Setting display fit mode: {}", fit));
                    let _ = wallpaper_webview.evaluate_script(&format!("setFit('{}');", fit));
                    let mut state = wp_state.borrow_mut();
                    state.fit_mode = fit.to_string();
                    save_config(&state);
                    drop(state);

                    let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                    if let Some(wv) = dashboard_webview.borrow().as_ref() {
                        let _ = wv.evaluate_script(&script);
                    }

                } else if let Some(&brightness) = controls.brightness_map.get(&event_id) {
                    log_msg(&format!("[ArcPaper] Setting brightness: {}", brightness));
                    let brightness_val = brightness as f32 / 100.0;
                    let _ = wallpaper_webview.evaluate_script(&format!("setBrightness({});", brightness_val));
                    let mut state = wp_state.borrow_mut();
                    state.brightness = brightness;
                    save_config(&state);
                    drop(state);

                    let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                    if let Some(wv) = dashboard_webview.borrow().as_ref() {
                        let _ = wv.evaluate_script(&script);
                    }

                } else if let Some(selected_wallpaper) = controls.wallpaper_map.get(&event_id).cloned() {
                    log_msg(&format!(
                        "[ArcPaper] Switching wallpaper from sub-menu: {}",
                        selected_wallpaper
                    ));
                    play_wallpaper(&wallpaper_webview, &selected_wallpaper);
                    let mut state = wp_state.borrow_mut();
                    state.active_video = Some(selected_wallpaper.clone());
                    state.is_manually_paused = false;
                    is_auto_paused = false;
                    save_config(&state);
                    let is_paused = state.is_manually_paused;
                    drop(state);

                    // Rebuild tray menu
                    let (new_menu, new_controls) = build_tray_menu(&saved_wallpapers_dir, is_paused);
                    let _ = tray_icon.set_menu(Some(Box::new(new_menu)));
                    controls = new_controls;

                    // Refresh dashboard if open
                    let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                    if let Some(wv) = dashboard_webview.borrow().as_ref() {
                        let _ = wv.evaluate_script(&script);
                    }
                }
            }

            // ── IPC messages from Dashboard WebView ──────────────────────────
            Event::UserEvent(UserEvent::IpcMessage(json)) => {
                log_msg(&format!("[ArcPaper] IPC message received: {}", json));

                let parsed: serde_json::Value = match serde_json::from_str(&json) {
                    Ok(v) => v,
                    Err(e) => {
                        log_msg(&format!("[ArcPaper] IPC JSON parse error: {:?}", e));
                        return;
                    }
                };

                let action = parsed["action"]
                    .as_str()
                    .or_else(|| parsed["type"].as_str())
                    .unwrap_or("");

                match action {
                    "request_init_data" | "ready" => {
                        log_msg("[ArcPaper] IPC: Handling request_init_data / ready");
                        let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                        if let Some(wv) = dashboard_webview.borrow().as_ref() {
                            let _ = wv.evaluate_script(&script);
                        }
                    }

                    "drag" => {
                        if let Some(w) = dashboard_window.borrow().as_ref() {
                            let _ = w.drag_window();
                        }
                    }

                    "close" => {
                        if let Some(w) = dashboard_window.borrow().as_ref() {
                            w.set_visible(false);
                        }
                    }

                    "upload" => {
                        log_msg("[ArcPaper] IPC: Upload requested from Dashboard.");
                        if let Some(source_path) = rfd::FileDialog::new()
                            .add_filter("MP4 Video (*.mp4)", &["mp4"])
                            .set_title("Select MP4 Video File")
                            .pick_file()
                        {
                            if let Some(file_name) = source_path.file_name().and_then(|n| n.to_str()) {
                                let dest_path = saved_wallpapers_dir.join(file_name);
                                if let Err(e) = std::fs::copy(&source_path, &dest_path) {
                                    log_msg(&format!("[ArcPaper] Failed to copy video: {:?}", e));
                                } else {
                                    log_msg(&format!("[ArcPaper] Video copied (IPC): {:?}", dest_path));
                                    play_wallpaper(&wallpaper_webview, file_name);

                                    let mut state = wp_state.borrow_mut();
                                    state.active_video = Some(file_name.to_string());
                                    state.is_manually_paused = false;
                                    is_auto_paused = false;
                                    save_config(&state);
                                    let is_paused = state.is_manually_paused;
                                    drop(state);

                                    // Rebuild tray menu
                                    let (new_menu, new_controls) = build_tray_menu(&saved_wallpapers_dir, is_paused);
                                    let _ = tray_icon.set_menu(Some(Box::new(new_menu)));
                                    controls = new_controls;

                                    // Refresh dashboard
                                    let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                                    if let Some(wv) = dashboard_webview.borrow().as_ref() {
                                        let _ = wv.evaluate_script(&script);
                                    }
                                }
                            }
                        }
                    }

                    "apply" => {
                        let filename = parsed["filename"].as_str().unwrap_or("").to_string();
                        let speed = parsed["speed"].as_f64().unwrap_or(1.0) as f32;
                        let brightness = parsed["brightness"].as_u64().unwrap_or(100) as u32;
                        let fit = parsed["fit"].as_str().unwrap_or("cover").to_string();

                        if !filename.is_empty() {
                            log_msg(&format!(
                                "[ArcPaper] IPC Apply: file='{}' speed={} brightness={} fit='{}'",
                                filename, speed, brightness, fit
                            ));

                            play_wallpaper(&wallpaper_webview, &filename);
                            let _ = wallpaper_webview.evaluate_script(&format!("setSpeed({});", speed));
                            let brightness_val = brightness as f32 / 100.0;
                            let _ = wallpaper_webview.evaluate_script(&format!("setBrightness({});", brightness_val));
                            let _ = wallpaper_webview.evaluate_script(&format!("setFit('{}');", js_escape(&fit)));

                            let mut state = wp_state.borrow_mut();
                            state.active_video = Some(filename.clone());
                            state.speed = speed;
                            state.brightness = brightness;
                            state.fit_mode = fit.clone();
                            state.is_manually_paused = false;
                            is_auto_paused = false;
                            save_config(&state);
                            let is_paused = state.is_manually_paused;
                            drop(state);

                            // Rebuild tray menu (in case wallpaper list changed)
                            let (new_menu, new_controls) = build_tray_menu(&saved_wallpapers_dir, is_paused);
                            let _ = tray_icon.set_menu(Some(Box::new(new_menu)));
                            controls = new_controls;

                            // Refresh dashboard to update ACTIVE badge
                            let script = build_update_gallery_script(&saved_wallpapers_dir, &wp_state.borrow());
                            if let Some(wv) = dashboard_webview.borrow().as_ref() {
                                let _ = wv.evaluate_script(&script);
                            }
                        }
                    }

                    "toggle_pause" => {
                        log_msg("[ArcPaper] IPC: Toggle pause/play on wallpaper.");
                        let mut state = wp_state.borrow_mut();
                        state.is_manually_paused = !state.is_manually_paused;
                        if state.is_manually_paused {
                            is_auto_paused = false;
                            let _ = wallpaper_webview.evaluate_script("video.pause();");
                        } else {
                            is_auto_paused = false;
                            let _ = wallpaper_webview.evaluate_script("video.play();");
                        }
                        save_config(&state);
                        let is_paused = state.is_manually_paused;
                        drop(state);

                        // Rebuild tray menu to update Pause / Resume label
                        let (new_menu, new_controls) = build_tray_menu(&saved_wallpapers_dir, is_paused);
                        let _ = tray_icon.set_menu(Some(Box::new(new_menu)));
                        controls = new_controls;
                    }

                    other => {
                        log_msg(&format!("[ArcPaper] IPC: Unknown message type '{}'", other));
                    }
                }
            }

            // ── Window events ────────────────────────────────────────────────
            Event::WindowEvent { window_id, event, .. } => {
                // Wallpaper window: only handle resize (close exits app)
                if window_id == wallpaper_window.id() {
                    match event {
                        WindowEvent::CloseRequested => {
                            log_msg("[ArcPaper] Wallpaper CloseRequested. Exiting...");
                            *control_flow = ControlFlow::Exit;
                        }
                        WindowEvent::Resized(new_size) => {
                            let _ = wallpaper_webview.set_bounds(wry::Rect {
                                position: wry::dpi::Position::Physical(PhysicalPosition::new(0, 0)),
                                size: wry::dpi::Size::Physical(PhysicalSize::new(
                                    new_size.width,
                                    new_size.height,
                                )),
                            });
                        }
                        _ => {}
                    }
                } else {
                    // Dashboard window events
                    let is_dashboard = dashboard_window_id
                        .borrow()
                        .map(|id| id == window_id)
                        .unwrap_or(false);

                    if is_dashboard {
                        if let WindowEvent::CloseRequested = event {
                            log_msg("[ArcPaper] Dashboard CloseRequested — hiding (not exiting).");
                            if let Some(w) = dashboard_window.borrow().as_ref() {
                                w.set_visible(false);
                            }
                        }
                    }
                }
            }

            // ── Fallback polling ─────────────────────────────────────────────
            Event::MainEventsCleared => {
                while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
                    let _ = proxy_ipc.send_event(UserEvent::TrayMenu(menu_event));
                }
            }

            _ => {}
        }
    });
}
