# ArcPaper 🎬⚡

> An ultra-lightweight, high-performance native video wallpaper engine for Windows, written in Rust and powered by `libmpv` Direct3D 11 hardware acceleration.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange.svg?logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Windows_10%20%2F%2011-0078D6.svg?logo=windows)](https://microsoft.com/windows)
[![Memory Usage](https://img.shields.io/badge/Memory_Usage-~10--20_MB-brightgreen.svg)]()
[![Zero WebView2](https://img.shields.io/badge/Web_Runtimes-0_(Native_Only)-success.svg)]()

---

## ✨ Features

- **⚡ Ultra-Low Resource Footprint**:
  - Consumes only **~10 – 20 MB RAM** while actively playing (dropping below 10 MB when paused).
  - Minimal CPU usage (< 2%) via pure Direct3D 11 GPU zero-copy hardware decoding (`d3d11va`).
  - **Zero WebView2 / Chromium instances** — completely native architecture.
- **🖥️ Desktop WorkerW Integration**:
  - Seamlessly renders behind Windows desktop icons using native Shell messaging (`Progman` / `WorkerW`).
  - Supports full desktop interaction without interfering with desktop icons or shortcuts.
- **🔍 Per-Monitor DPI Aware V2**:
  - Native pixel-perfect rendering for 1080p, 2K, and 4K displays.
  - No blurry DWM stretching or scaling artifacts on 125%/150% Windows display scaling.
- **🔄 True 100% Seamless Looping**:
  - Uses `libmpv`'s internal zero-delay loop engine with `display-desync`, `hr-seek=no`, and pre-fetched demuxer buffers.
  - Zero stutter, zero audio/video desync, and no disk I/O spikes when reaching 00:00.
- **⏸️ Smart Auto-Pause**:
  - Automatically pauses video playback and flushes working set memory when any fullscreen application or maximized game covers the screen.
  - Resumes instantly when the desktop becomes visible again.
- **🎛️ System Tray Quick Controls**:
  - **Wallpaper Switcher**: Switch between saved wallpapers on the fly.
  - **Upload Wallpaper**: Add any `.mp4` file via native Windows file picker.
  - **Playback Speed**: Adjust speed to `0.5x`, `1.0x`, or `1.5x`.
  - **Screen Fit Modes**: `Cover` (fill & crop), `Fill` (stretch), and `Contain` (letterbox).
  - **Brightness Control**: Quick dimming presets (`100%`, `75%`, `50%`).
  - **Manual Pause / Resume**: Freeze playback at any time.
- **🛡️ Single Instance Protected**:
  - Enforced via Windows Kernel Named Mutex to prevent multiple processes from conflicting.

---

## 📊 Performance Comparison

| Feature / Metric | Typical Electron/Web Wallpaper Apps | Wallpaper Engine (Standard) | **ArcPaper** |
| :--- | :---: | :---: | :---: |
| **RAM (Playing)** | 300 – 600 MB | 30 – 60 MB | **~10 – 20 MB** |
| **RAM (Paused)** | 150 – 300 MB | 15 – 30 MB | **< 10 MB** |
| **Executable Size** | 80 – 150 MB | 10 – 25 MB | **~400 KB** |
| **Render Engine** | Chromium / WebView2 | Custom Direct3D | **Native libmpv (Direct3D 11)** |
| **Hardware Decoding** | Inconsistent / Software | Direct3D / DXVA | **Zero-Copy VRAM (`d3d11va`)** |
| **Runtime Dependencies** | Chromium, Node, .NET | Visual C++ Redist | **Standalone `mpv-2.dll`** |

---

## 🚀 Getting Started

### Prerequisites

- **Windows 10 / 11** (64-bit)
- [Rust toolchain](https://rustup.rs/) (edition 2024 / 1.85+)
- `mpv-2.dll` placed in the application root directory

### Installation & Build

1. **Clone the repository:**
   ```bash
   git clone https://github.com/ArtixForLiving/ArcPaper.git
   cd ArcPaper
   ```

2. **Ensure `mpv-2.dll` is present:**
   Place a compatible 64-bit `mpv-2.dll` build in the root folder `ArcPaper/`.

3. **Build optimized release binary:**
   ```bash
   cargo build --release
   ```
   The compiled executable will be located at `target/release/arc_paper.exe`.

4. **Run ArcPaper:**
   ```bash
   cargo run --release
   ```
   Or double-click `arc_paper.exe`. An icon will appear in your Windows System Tray.

---

## 📂 Directory Structure

```text
ArcPaper/
├── SavedWallpapers/       # Directory where active and uploaded MP4 wallpapers are stored
├── src/
│   ├── main.rs            # Application entry, event loop, tray menus & auto-pause watcher
│   ├── player.rs          # Dynamic libmpv C-API bindings & player controller
│   └── workerw.rs         # Windows Desktop WorkerW hooking & parent attachment
├── include/mpv/           # libmpv C headers
├── config.json            # Persisted user settings (active wallpaper, speed, brightness, fit)
├── Cargo.toml             # Project manifest & optimization profiles
└── .gitignore             # Git exclusion rules (ignores heavy DLLs, videos, targets)
```

---

## ⚙️ Configuration (`config.json`)

ArcPaper saves user preferences automatically into `config.json`:

```json
{
  "active_video": "your_wallpaper.mp4",
  "speed": 1.0,
  "brightness": 100,
  "fit_mode": "cover",
  "is_manually_paused": false
}
```

---

## 🛠️ Built With

- [Rust](https://www.rust-lang.org/) - Systems programming language
- [mpv](https://mpv.io/) - Media player core for zero-copy hardware-accelerated video decoding
- [tao](https://github.com/tauri-apps/tao) - Cross-platform window creation and event loop
- [tray-icon](https://github.com/tauri-apps/tray-icon) - Windows system tray integration
- [windows-rs](https://github.com/microsoft/windows-rs) - Official Microsoft Win32 API bindings for Rust

---

## 📜 License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for details.
