use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{PCSTR, PCWSTR, w};

pub type MpvHandle = c_void;

type FnMpvCreate = unsafe extern "C" fn() -> *mut MpvHandle;
type FnMpvInitialize = unsafe extern "C" fn(*mut MpvHandle) -> c_int;
type FnMpvSetOptionString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type FnMpvSetPropertyString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type FnMpvGetProperty =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, c_int, *mut c_void) -> c_int;
type FnMpvGetPropertyString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char) -> *mut c_char;
type FnMpvFree = unsafe extern "C" fn(*mut c_void);
type FnMpvCommand = unsafe extern "C" fn(*mut MpvHandle, *mut *const c_char) -> c_int;
type FnMpvCommandString = unsafe extern "C" fn(*mut MpvHandle, *const c_char) -> c_int;
type FnMpvTerminateDestroy = unsafe extern "C" fn(*mut MpvHandle);
type FnMpvErrorString = unsafe extern "C" fn(c_int) -> *const c_char;

#[allow(dead_code)]
pub struct MpvApi {
    pub mpv_create: FnMpvCreate,
    pub mpv_initialize: FnMpvInitialize,
    pub mpv_set_option_string: FnMpvSetOptionString,
    pub mpv_set_property_string: FnMpvSetPropertyString,
    pub mpv_get_property: FnMpvGetProperty,
    pub mpv_get_property_string: FnMpvGetPropertyString,
    pub mpv_free: FnMpvFree,
    pub mpv_command: FnMpvCommand,
    pub mpv_command_string: FnMpvCommandString,
    pub mpv_terminate_destroy: FnMpvTerminateDestroy,
    pub mpv_error_string: FnMpvErrorString,
}

impl MpvApi {
    /// Attempts to dynamically load `mpv-2.dll` or `mpv.dll` from the application directory,
    /// target build directories, or system search path.
    pub fn load() -> Result<Self, String> {
        let mut candidate_paths = vec![
            PathBuf::from("D:\\Arc Paper\\dist\\ArcPaper\\libmpv-2.dll"),
            PathBuf::from("D:\\Arc Paper\\dist\\ArcPaper\\mpv-2.dll"),
            PathBuf::from("D:\\Arc Paper\\libmpv-2.dll"),
            PathBuf::from("D:\\Arc Paper\\mpv-2.dll"),
            PathBuf::from("D:\\Arc Paper\\target\\release\\libmpv-2.dll"),
            PathBuf::from("D:\\Arc Paper\\target\\release\\mpv-2.dll"),
            PathBuf::from("D:\\Arc Paper\\target\\debug\\libmpv-2.dll"),
            PathBuf::from("D:\\Arc Paper\\target\\debug\\mpv-2.dll"),
            PathBuf::from("libmpv-2.dll"),
            PathBuf::from("mpv-2.dll"),
            PathBuf::from("mpv.dll"),
        ];

        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                candidate_paths.insert(0, parent.join("libmpv-2.dll"));
                candidate_paths.insert(1, parent.join("mpv-2.dll"));
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidate_paths.insert(0, cwd.join("libmpv-2.dll"));
            candidate_paths.insert(1, cwd.join("mpv-2.dll"));
        }

        let mut hmodule = None;
        for path in &candidate_paths {
            if path.exists() {
                let wide: Vec<u16> = path
                    .as_os_str()
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                if let Ok(handle) = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) } {
                    hmodule = Some(handle);
                    break;
                }
            }
        }

        if hmodule.is_none() {
            if let Ok(handle) = unsafe { LoadLibraryW(w!("libmpv-2.dll")) } {
                hmodule = Some(handle);
            } else if let Ok(handle) = unsafe { LoadLibraryW(w!("mpv-2.dll")) } {
                hmodule = Some(handle);
            } else if let Ok(handle) = unsafe { LoadLibraryW(w!("mpv.dll")) } {
                hmodule = Some(handle);
            }
        }

        let module = match hmodule {
            Some(m) => m,
            None => {
                return Err(
                    "libmpv-2.dll or mpv-2.dll not found. Please place libmpv-2.dll in the application directory."
                        .to_string(),
                );
            }
        };

        unsafe {
            let get_sym = |name: &[u8]| -> Result<*const (), String> {
                let c_name = PCSTR(name.as_ptr());
                GetProcAddress(module, c_name)
                    .map(|addr| addr as *const ())
                    .ok_or_else(|| {
                        format!(
                            "Symbol {:?} not found in mpv DLL",
                            String::from_utf8_lossy(name)
                        )
                    })
            };

            Ok(MpvApi {
                mpv_create: std::mem::transmute(get_sym(b"mpv_create\0")?),
                mpv_initialize: std::mem::transmute(get_sym(b"mpv_initialize\0")?),
                mpv_set_option_string: std::mem::transmute(get_sym(b"mpv_set_option_string\0")?),
                mpv_set_property_string: std::mem::transmute(get_sym(
                    b"mpv_set_property_string\0",
                )?),
                mpv_get_property: std::mem::transmute(get_sym(b"mpv_get_property\0")?),
                mpv_get_property_string: std::mem::transmute(get_sym(
                    b"mpv_get_property_string\0",
                )?),
                mpv_free: std::mem::transmute(get_sym(b"mpv_free\0")?),
                mpv_command: std::mem::transmute(get_sym(b"mpv_command\0")?),
                mpv_command_string: std::mem::transmute(get_sym(b"mpv_command_string\0")?),
                mpv_terminate_destroy: std::mem::transmute(get_sym(b"mpv_terminate_destroy\0")?),
                mpv_error_string: std::mem::transmute(get_sym(b"mpv_error_string\0")?),
            })
        }
    }
}

#[allow(dead_code)]
pub struct MpvPlayer {
    api: MpvApi,
    handle: *mut MpvHandle,
    wid: HWND,
}

#[allow(dead_code)]
impl MpvPlayer {
    /// Creates and initializes a new mpv instance attached to `wid` with hardware acceleration.
    pub fn new(wid: HWND) -> Result<Self, String> {
        let api = MpvApi::load()?;
        let handle = unsafe { (api.mpv_create)() };
        if handle.is_null() {
            return Err("Failed to create mpv instance via mpv_create".to_string());
        }

        let player = MpvPlayer { api, handle, wid };

        // 1. --wid=<HWND_WORKERW> (attach directly to WorkerW)
        let wid_num = wid.0 as usize;
        let wid_str = format!("{}", wid_num);
        player.set_option("wid", &wid_str)?;

        // 2. Pure hardware acceleration: Zero-copy VRAM direct to GPU
        player.set_option("hwdec", "d3d11va")?;
        let _ = player.set_option("hwdec-codecs", "all");

        // 3. Video output & window configuration (100% sharp visual quality, minimal RAM)
        player.set_option("vo", "gpu")?;
        player.set_option("gpu-api", "d3d11")?;
        player.set_option("gpu-dumb-mode", "no")?;
        player.set_option("scale", "bilinear")?;
        player.set_option("dscale", "mitchell")?;
        player.set_option("keepaspect", "yes")?;
        player.set_option("panscan", "1.0")?;
        player.set_option("swapchain-depth", "1")?;
        player.set_option("force-window", "yes")?;
        player.set_option("pause", "no")?;

        // 4. Seamless loop transition (100% internal libmpv zero-stutter loop)
        player.set_option("loop-file", "inf")?;
        player.set_option("keep-open", "yes")?;
        player.set_option("force-seekable", "yes")?;
        player.set_option("video-sync", "display-desync")?;
        player.set_option("hr-seek", "no")?;

        // 5. Total elimination of audio decoding and subtitles
        player.set_option("audio", "no")?;
        player.set_option("aid", "no")?;
        player.set_option("sid", "no")?;

        // 6. Disable all internal libmpv logging and OSD to prevent memory accumulation
        let _ = player.set_option("msg-level", "all=no");
        let _ = player.set_option("osd-level", "0");
        let _ = player.set_option("terminal", "no");
        let _ = player.set_option("msg-color", "no");

        // 7. Trimmed Demuxer Buffer (Total 5 MB: 3 MB forward + 2 MB backward, static memory ceiling)
        player.set_option("cache", "yes")?;
        player.set_option("cache-on-disk", "no")?;
        player.set_option("demuxer-max-bytes", "3145728")?; // 3 MB forward buffer
        player.set_option("demuxer-max-back-bytes", "2097152")?; // 2 MB backward buffer
        player.set_option("demuxer-donate-buffer", "no")?;
        player.set_option("demuxer-readahead-secs", "1.0")?;
        player.set_option("vd-lavc-threads", "1")?;
        player.set_option("vd-lavc-dr", "yes")?; // Direct Rendering straight to GPU
        let _ = player.set_option("icc-profile-auto", "no");
        let _ = player.set_option("gpu-shader-cache-dir", "");

        // Initialize mpv engine
        let err = unsafe { (player.api.mpv_initialize)(player.handle) };
        if err < 0 {
            let err_str = player.error_str(err);
            return Err(format!(
                "Failed to initialize mpv (code {}): {}",
                err, err_str
            ));
        }

        Ok(player)
    }

    pub fn set_option(&self, name: &str, value: &str) -> Result<(), String> {
        let c_name = CString::new(name).map_err(|e| e.to_string())?;
        let c_val = CString::new(value).map_err(|e| e.to_string())?;
        let err = unsafe {
            (self.api.mpv_set_option_string)(self.handle, c_name.as_ptr(), c_val.as_ptr())
        };
        if err < 0 {
            Err(format!(
                "Error setting option '{}'='{}': {}",
                name,
                value,
                self.error_str(err)
            ))
        } else {
            Ok(())
        }
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<(), String> {
        let c_name = CString::new(name).map_err(|e| e.to_string())?;
        let c_val = CString::new(value).map_err(|e| e.to_string())?;
        let err = unsafe {
            (self.api.mpv_set_property_string)(self.handle, c_name.as_ptr(), c_val.as_ptr())
        };
        if err < 0 {
            Err(format!(
                "Error setting property '{}'='{}': {}",
                name,
                value,
                self.error_str(err)
            ))
        } else {
            Ok(())
        }
    }

    /// Executes an MPV command with the provided slice of string arguments.
    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let c_args: Result<Vec<CString>, _> = args.iter().map(|&s| CString::new(s)).collect();
        let c_args = c_args.map_err(|e| e.to_string())?;
        let mut ptrs: Vec<*const c_char> = c_args.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let err = unsafe { (self.api.mpv_command)(self.handle, ptrs.as_mut_ptr()) };
        if err < 0 {
            Err(format!(
                "Error executing command {:?}: {}",
                args,
                self.error_str(err)
            ))
        } else {
            Ok(())
        }
    }

    /// Applies a virtual timeline ring buffer using MPV's `edl://` protocol.
    /// This stitches `ring_size` loop slots into a single virtual timeline without multi-item playlist resets,
    /// preventing D3D11VA hardware decoder re-initialization, CPU spikes, and memory bloating.
    pub fn apply_ring_buffer(&self, video_path: &str, ring_size: usize) -> Result<(), String> {
        let clean_path = video_path.replace('\\', "/");
        let path_len = clean_path.len();
        let edl_entry = format!("%{}%{}", path_len, clean_path);

        let count = if ring_size == 0 { 10 } else { ring_size };
        let mut segments: Vec<&str> = Vec::with_capacity(count);
        for _ in 0..count {
            segments.push(&edl_entry);
        }
        let edl_url = format!("edl://{}", segments.join(";"));

        let c_loadfile = CString::new("loadfile").map_err(|e| e.to_string())?;
        let c_url = CString::new(edl_url).map_err(|e| e.to_string())?;
        let c_replace = CString::new("replace").map_err(|e| e.to_string())?;
        let mut args: [*const c_char; 4] = [
            c_loadfile.as_ptr(),
            c_url.as_ptr(),
            c_replace.as_ptr(),
            std::ptr::null(),
        ];
        let err = unsafe { (self.api.mpv_command)(self.handle, args.as_mut_ptr()) };
        if err < 0 {
            return Err(format!(
                "Error loading EDL virtual timeline '{}': {}",
                video_path,
                self.error_str(err)
            ));
        }

        // Configure single-stream seamless infinite loop properties
        let _ = self.set_property("loop-file", "inf");
        let _ = self.set_property("keep-open", "yes");
        let _ = self.set_property("scale", "bilinear");
        let _ = self.set_property("dscale", "mitchell");
        let _ = self.set_property("keepaspect", "yes");
        let _ = self.set_property("panscan", "1.0");
        let _ = self.set_property("video-sync", "display-desync");
        let _ = self.set_property("hr-seek", "no");
        let _ = self.set_property("aid", "no");
        let _ = self.set_property("sid", "no");
        let _ = self.set_property("msg-level", "all=no");
        let _ = self.set_property("osd-level", "0");

        Ok(())
    }

    /// Changes the active wallpaper seamlessly using MPV's `loadfile <path> replace`.
    /// This replaces the current video in-place without destroying or recreating the HWND/swapchain,
    /// completely eliminating black flashes or window flicker.
    pub fn change_wallpaper(&self, video_path: &str) -> Result<(), String> {
        let clean_path = video_path.replace('\\', "/");
        let c_loadfile = CString::new("loadfile").map_err(|e| e.to_string())?;
        let c_path = CString::new(clean_path).map_err(|e| e.to_string())?;
        let c_replace = CString::new("replace").map_err(|e| e.to_string())?;
        let mut args: [*const c_char; 4] = [
            c_loadfile.as_ptr(),
            c_path.as_ptr(),
            c_replace.as_ptr(),
            std::ptr::null(),
        ];
        let err = unsafe { (self.api.mpv_command)(self.handle, args.as_mut_ptr()) };
        if err < 0 {
            Err(format!(
                "Error changing wallpaper '{}': {}",
                video_path,
                self.error_str(err)
            ))
        } else {
            let _ = self.set_property("scale", "bilinear");
            let _ = self.set_property("dscale", "mitchell");
            let _ = self.set_property("keepaspect", "yes");
            let _ = self.set_property("panscan", "1.0");
            let _ = self.set_property("video-sync", "display-desync");
            let _ = self.set_property("hr-seek", "no");
            let _ = self.set_property("loop-file", "inf");
            let _ = self.set_property("keep-open", "yes");
            let _ = self.set_property("aid", "no");
            let _ = self.set_property("sid", "no");
            Ok(())
        }
    }

    /// Plays a video wallpaper using the `loadfile <path> replace` command.
    pub fn play_wallpaper(&self, video_path: &str) -> Result<(), String> {
        self.change_wallpaper(video_path)
    }

    pub fn set_pause(&self, paused: bool) -> Result<(), String> {
        self.set_property("pause", if paused { "yes" } else { "no" })
    }

    pub fn set_speed(&self, speed: f32) -> Result<(), String> {
        self.set_property("speed", &format!("{:.2}", speed))
    }

    pub fn set_brightness(&self, brightness: u32) -> Result<(), String> {
        // mpv brightness range is -100 to 100, where 0 is default / neutral (brightness 100 in UI)
        let val = (brightness as i32) - 100;
        self.set_property("brightness", &format!("{}", val))
    }

    pub fn set_fit(&self, fit_mode: &str) -> Result<(), String> {
        // "cover" -> keepaspect=yes + video-unscaled=no + panscan=1.0
        // "contain" -> keepaspect=yes + video-unscaled=no + panscan=0.0
        // "fill" -> keepaspect=no
        match fit_mode {
            "fill" => {
                self.set_property("keepaspect", "no")?;
                self.set_property("panscan", "0.0")?;
            }
            "contain" => {
                self.set_property("keepaspect", "yes")?;
                self.set_property("panscan", "0.0")?;
            }
            _ => {
                // Default: cover (fill & crop with aspect ratio preserved)
                self.set_property("keepaspect", "yes")?;
                self.set_property("panscan", "1.0")?;
            }
        }
        Ok(())
    }

    pub fn get_property_f64(&self, name: &str) -> Result<f64, String> {
        let c_name = CString::new(name).map_err(|e| e.to_string())?;
        let mut val: f64 = 0.0;
        let err = unsafe {
            (self.api.mpv_get_property)(
                self.handle,
                c_name.as_ptr(),
                5, // MPV_FORMAT_DOUBLE
                &mut val as *mut f64 as *mut c_void,
            )
        };
        if err < 0 {
            Err(format!(
                "Error getting property '{}': {}",
                name,
                self.error_str(err)
            ))
        } else {
            Ok(val)
        }
    }

    pub fn get_property_string(&self, name: &str) -> Result<String, String> {
        let c_name = CString::new(name).map_err(|e| e.to_string())?;
        let ptr = unsafe { (self.api.mpv_get_property_string)(self.handle, c_name.as_ptr()) };
        if ptr.is_null() {
            Err(format!("Property '{}' returned null", name))
        } else {
            let val = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
            unsafe { (self.api.mpv_free)(ptr as *mut c_void) };
            Ok(val)
        }
    }

    pub fn get_native_fps(&self) -> Option<f64> {
        if let Ok(fps) = self.get_property_f64("container-fps") {
            if fps > 1.0 {
                return Some(fps);
            }
        }
        if let Ok(fps) = self.get_property_f64("estimated-vf-fps") {
            if fps > 1.0 {
                return Some(fps);
            }
        }
        if let Ok(fps) = self.get_property_f64("fps") {
            if fps > 1.0 {
                return Some(fps);
            }
        }
        None
    }

    pub fn detect_native_fps(&self) -> Option<f64> {
        for _ in 0..40 {
            if let Some(fps) = self.get_native_fps() {
                return Some(fps);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        None
    }

    pub fn apply_fps_limit(&self, target_fps: Option<u32>) -> Result<(), String> {
        let _ = self.set_property("vf", "");
        match target_fps {
            Some(fps) if fps <= 15 && fps > 0 => {
                let _ = self.set_property("vd-lavc-skipframe", "nonref");
                let _ = self.set_property("vd-lavc-skiploopfilter", "all");
                let _ = self.set_property("framedrop", "decoder+vo");
            }
            Some(fps) if fps > 15 => {
                let _ = self.set_property("vd-lavc-skipframe", "default");
                let _ = self.set_property("vd-lavc-skiploopfilter", "nonref");
                let _ = self.set_property("framedrop", "vo");
            }
            _ => {
                let _ = self.set_property("vd-lavc-skipframe", "default");
                let _ = self.set_property("vd-lavc-skiploopfilter", "default");
                let _ = self.set_property("framedrop", "no");
            }
        }
        Ok(())
    }

    fn error_str(&self, code: c_int) -> String {
        unsafe {
            let ptr = (self.api.mpv_error_string)(code);
            if ptr.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        }
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.api.mpv_terminate_destroy)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

// MpvPlayer can be transferred between threads safely
unsafe impl Send for MpvPlayer {}

use std::sync::Mutex;

static GLOBAL_PLAYER: Mutex<Option<MpvPlayer>> = Mutex::new(None);

/// Initializes the global MPV player attached to `wid` (HWND of WorkerW)
pub fn init_player(wid: HWND) -> Result<(), String> {
    let player = MpvPlayer::new(wid)?;
    let mut lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    *lock = Some(player);
    Ok(())
}

/// Seamlessly changes the active wallpaper using MPV's `loadfile <path> replace`
pub fn change_wallpaper(video_path: &str) -> Result<(), String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.change_wallpaper(video_path)
    } else {
        Err("MPV Player is not initialized. Call init_player(wid) first.".to_string())
    }
}

/// Plays a wallpaper video using the global MPV player instance (aliases `change_wallpaper`)
#[allow(dead_code)]
pub fn play_wallpaper(video_path: &str) -> Result<(), String> {
    change_wallpaper(video_path)
}

pub fn set_pause(paused: bool) -> Result<(), String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.set_pause(paused)
    } else {
        Ok(())
    }
}

pub fn set_speed(speed: f32) -> Result<(), String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.set_speed(speed)
    } else {
        Ok(())
    }
}

pub fn set_brightness(brightness: u32) -> Result<(), String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.set_brightness(brightness)
    } else {
        Ok(())
    }
}

pub fn set_fit(fit_mode: &str) -> Result<(), String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.set_fit(fit_mode)
    } else {
        Ok(())
    }
}

/// Applies a ring buffer containing `ring_size` loop slots using the global MPV player instance.
pub fn apply_ring_buffer(video_path: &str, ring_size: usize) -> Result<(), String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.apply_ring_buffer(video_path, ring_size)
    } else {
        Err("MPV Player is not initialized. Call init_player(wid) first.".to_string())
    }
}

/// Executes an arbitrary MPV command on the global MPV player instance.
#[allow(dead_code)]
pub fn command(args: &[&str]) -> Result<(), String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.command(args)
    } else {
        Err("MPV Player is not initialized. Call init_player(wid) first.".to_string())
    }
}

#[allow(dead_code)]
pub fn get_property_f64(name: &str) -> Result<f64, String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.get_property_f64(name)
    } else {
        Err("MPV Player is not initialized.".to_string())
    }
}

#[allow(dead_code)]
pub fn get_property_string(name: &str) -> Result<String, String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.get_property_string(name)
    } else {
        Err("MPV Player is not initialized.".to_string())
    }
}

#[allow(dead_code)]
pub fn get_native_fps() -> Option<f64> {
    let lock = GLOBAL_PLAYER.lock().ok()?;
    if let Some(ref player) = *lock {
        player.get_native_fps()
    } else {
        None
    }
}

pub fn detect_native_fps() -> Option<f64> {
    let lock = GLOBAL_PLAYER.lock().ok()?;
    if let Some(ref player) = *lock {
        player.detect_native_fps()
    } else {
        None
    }
}

pub fn apply_fps_limit(target_fps: Option<u32>) -> Result<(), String> {
    let lock = GLOBAL_PLAYER.lock().map_err(|e| e.to_string())?;
    if let Some(ref player) = *lock {
        player.apply_fps_limit(target_fps)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mpv_player_lifecycle() {
        // Load API and verify symbol resolution
        let api = MpvApi::load();
        assert!(api.is_ok(), "Failed to load MpvApi: {:?}", api.err());

        // Test module-level init_player and play_wallpaper
        let init_res = init_player(HWND::default());
        assert!(init_res.is_ok(), "Failed init_player: {:?}", init_res.err());

        let saved_dir = if std::path::Path::new("SavedWallpapers").exists() {
            std::path::PathBuf::from("SavedWallpapers")
        } else {
            std::path::PathBuf::from("D:\\Arc Paper\\SavedWallpapers")
        };

        if let Ok(files) = std::fs::read_dir(saved_dir) {
            for entry in files.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path
                    .extension()
                    .map_or(false, |ext| ext.eq_ignore_ascii_case("mp4"))
                {
                    let res = play_wallpaper(&path.to_string_lossy());
                    println!("play_wallpaper result for {:?}: {:?}", path, res);
                    assert!(res.is_ok());

                    // Test dynamic ring buffer with 5, 10, and 20 loops
                    assert!(apply_ring_buffer(&path.to_string_lossy(), 5).is_ok());
                    assert!(apply_ring_buffer(&path.to_string_lossy(), 10).is_ok());
                    assert!(apply_ring_buffer(&path.to_string_lossy(), 20).is_ok());

                    // Test native FPS detection
                    let fps = detect_native_fps();
                    println!("detect_native_fps result for {:?}: {:?}", path, fps);

                    // Test FPS limit application across all profiles
                    assert!(apply_fps_limit(Some(30)).is_ok());
                    assert!(apply_fps_limit(Some(20)).is_ok());
                    assert!(apply_fps_limit(Some(15)).is_ok());
                    assert!(apply_fps_limit(None).is_ok());
                    break;
                }
            }
        }
    }

    #[test]
    fn test_native_fps_detection_timing() {
        let _ = init_player(HWND::default());
        let saved_dir = if std::path::Path::new("SavedWallpapers").exists() {
            std::path::PathBuf::from("SavedWallpapers")
        } else {
            std::path::PathBuf::from("D:\\Arc Paper\\SavedWallpapers")
        };
        if let Ok(files) = std::fs::read_dir(saved_dir) {
            for entry in files.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext.eq_ignore_ascii_case("mp4")) {
                    let _ = apply_ring_buffer(&path.to_string_lossy(), 10);
                    for i in 1..=40 {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        let c_fps = get_property_f64("container-fps");
                        let v_fps = get_property_f64("estimated-vf-fps");
                        println!("Attempt {}: container-fps={:?}, estimated-vf-fps={:?}", i, c_fps, v_fps);
                        if c_fps.is_ok() || v_fps.is_ok() {
                            break;
                        }
                    }
                    break;
                }
            }
        }
    }
}
