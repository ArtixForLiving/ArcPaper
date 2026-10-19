# ArcPaper

<p align="center">
  <strong>Ultra-lightweight, hardware-accelerated Windows live wallpaper engine built with Rust and libmpv.</strong>
</p>

<p align="center">
  <a href="https://github.com/ArtixForLiving/ArcPaper/releases"><img src="https://img.shields.io/github/v/release/ArtixForLiving/ArcPaper?color=00f2fe&style=for-the-badge" alt="Release v1.5.0" /></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Language-Rust%202024-orange.svg?style=for-the-badge&logo=rust" alt="Rust" /></a>
  <a href="https://www.microsoft.com/windows"><img src="https://img.shields.io/badge/Platform-Windows%2011%20%2F%2010-0078d4.svg?style=for-the-badge&logo=windows" alt="Windows" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-10b981.svg?style=for-the-badge" alt="License MIT" /></a>
</p>

---

## ⚡ Overview

**ArcPaper** is a high-performance Windows live wallpaper engine engineered with **Rust**, **Win32 API**, and **libmpv**. Unlike bloated wallpaper applications that consume hundreds of megabytes of RAM and multiple CPU cores, ArcPaper attaches directly to the Windows desktop shell (`WorkerW`) with **hardware-accelerated zero-copy Direct3D 11 rendering**, delivering butter-smooth 60 FPS playback with virtually zero resource footprint.

---

## ✨ Features

- **🚀 Ultra-Low Resource Consumption**
  - Consumes **< 10 MB RAM** in paused state and minimal memory during 4K/60FPS playback.
  - Typical **0–1% CPU usage** via direct GPU hardware video decoding (`d3d11va`).

- **🎮 Smart Auto-Pause for Games & Fullscreen Apps**
  - Automatically suspends video rendering and trims memory working set when any fullscreen or maximized window (e.g., games, browsers, IDEs) covers the screen.
  - Instantly resumes playback when returning to the desktop.

- **🔄 Seamless Gapless Video Looping**
  - Utilizes internal libmpv zero-stutter frame buffer streaming (`loadfile <path> replace`).
  - No black frames, micro-stutters, or audio glitches when transitioning or switching wallpapers.

- **💎 Per-Monitor High-DPI Crystal Clear Rendering**
  - Native Per-Monitor V2 DPI awareness ensures crisp physical pixel mapping across high-resolution displays (1080p, 1440p, 4K+).

- **🖱️ System Tray Control Center**
  - Right-click tray menu provides instant access to:
    - **Upload New Wallpaper** (file picker dialog)
    - **Select Active Wallpaper** (switch on-the-fly with live active indicators)
    - **Pause / Resume Wallpaper**
    - **Display & Visuals Submenu**:
      - **Playback Speed** (0.5x, 1.0x, 1.5x)
      - **Screen Fit Mode** (Cover, Fill, Contain)
      - **Brightness Control** (100%, 75%, 50%)
      - **Target FPS Profiles** (Unlimited, 30 FPS, 20 FPS, 15 FPS via `vd-lavc` skipframe)
      - **EDL Ring Buffer Looping** (5x, 10x, 20x virtual timeline loops)
    - **Open Wallpapers Folder** & **Open Logs**
    - **Check for Updates...** (non-blocking GitHub release check with native Win32 dialog)
    - **Start with Windows** (one-click toggle for automatic startup)
    - **Uninstall ArcPaper** (cleanly deregisters autostart and cleans up)

- **⚙️ Windows Auto-Start Integration**
  - Cleanly registers or unregisters from Windows Registry (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`) without external background services.

---

## 🛠️ Tech Stack

| Component | Technology | Description |
|---|---|---|
| **Language** | [Rust](https://www.rust-lang.org/) (2024 edition) | High performance, memory-safe native compiled binary |
| **Video Engine** | [libmpv](https://mpv.io/) (`mpv-2.dll`) | Zero-copy Direct3D 11 hardware decoding & rendering |
| **Desktop Hook** | Win32 API (`windows` crate) | Direct attachment to Windows `WorkerW` / `Progman` desktop layers |
| **System Tray** | [tray-icon](https://crates.io/crates/tray-icon) & [muda](https://crates.io/crates/muda) | Native non-blocking Windows notification area menu |
| **Auto-Start** | [winreg](https://crates.io/crates/winreg) | Clean HKCU registry persistence |

---

## 📥 Installation & Usage

### Quick Start (Portable)

1. Download the latest release package: [`ArcPaper-v1.5.0-win64.zip`](https://github.com/ArtixForLiving/ArcPaper/releases/latest).
2. Extract the archive to any folder (e.g., `C:\Tools\ArcPaper` or `D:\ArcPaper`).
3. Run `arc_paper.exe`.
4. Right-click the **ArcPaper** icon in your Windows System Tray to:
   - Click **"Upload New Wallpaper..."** to import your favorite `.mp4` video.
   - Check **"Start with Windows"** if you want ArcPaper to launch automatically upon login.

> **Tip:** You can also drop `.mp4` video files directly into the `SavedWallpapers` folder next to `arc_paper.exe`!

---

## 🏗️ Building from Source

### Prerequisites
- [Rust toolchain](https://rustup.rs/) (1.80+ or latest stable)
- Windows 10 / 11 64-bit
- `mpv-2.dll` / `libmpv-2.dll` placed in the root directory

### Compilation
```powershell
# Clone the repository
git clone https://github.com/ArtixForLiving/ArcPaper.git
cd ArcPaper

# Build optimized release binary
cargo build --release

# Run unit tests
cargo test
```

The resulting standalone executable will be located at `target/release/arc_paper.exe`.

---

## 📄 License

This project is licensed under the [MIT License](LICENSE.md).
