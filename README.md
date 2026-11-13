# ArcPaper

<p align="center">
  <strong>Ultra-lightweight, zero-copy hardware-accelerated Windows live wallpaper engine built with pure Rust and Direct3D 11.</strong>
</p>

<p align="center">
  <a href="https://github.com/ArtixForLiving/ArcPaper/releases"><img src="https://img.shields.io/github/v/release/ArtixForLiving/ArcPaper?color=00f2fe&style=for-the-badge" alt="Release v1.8.0" /></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Language-Rust%202024-orange.svg?style=for-the-badge&logo=rust" alt="Rust 2024" /></a>
  <a href="https://www.microsoft.com/windows"><img src="https://img.shields.io/badge/Platform-Windows%2011%20%2F%2010-0078d4.svg?style=for-the-badge&logo=windows" alt="Windows" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-10b981.svg?style=for-the-badge" alt="License MIT" /></a>
  <img src="https://img.shields.io/badge/Dependencies-Zero%20DLLs%20%2F%20~1.8%20MB-success.svg?style=for-the-badge" alt="Zero Dependencies" />
</p>

---

## ⚡ Overview

**ArcPaper** is a high-performance, standalone Windows live wallpaper engine engineered in **100% pure Rust**. Unlike traditional wallpaper engines that depend on heavy third-party media players, Electron runtimes, or hundreds of megabytes of bundled C-libraries, ArcPaper features a custom **native Direct3D 11 & Windows Media Foundation hardware video pipeline**.

ArcPaper renders video frames directly into GPU VRAM using high-performance flip-model swapchains (`DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`) and pins itself securely behind desktop icons (`WorkerW` / `Progman`). With built-in `HTTRANSPARENT` mouse pass-through, desktop icons, file selections, and shortcuts remain 100% responsive with zero input latency.

In **v1.8.0**, ArcPaper introduces **Power & Efficiency Awareness** — incorporating event-driven WinEvent hooks for sub-100 ms auto-pause response, comprehensive laptop battery and session lifecycle intelligence, an orthogonal `PauseReason` bitflags model, and Config Schema v3.

---

## 📊 Performance Benchmarks

| Metric | ArcPaper v1.8.0 | Traditional Wallpaper Engines |
|---|---|---|
| **Auto-Pause Detection Latency** | **< 100 ms (Event-driven WinEvent hook)** | 500–1000 ms (Polling loops) |
| **Idle / Auto-Pause RAM** | **~4–5 MB** (Working set & GPU trimmed) | 80–150 MB |
| **Active 1080p 60 FPS RAM** | **~30–35 MB** (1 monitor) | 180–350 MB |
| **CPU Usage (Active)** | **~0.3% – 0.5%** | 3.0% – 12.0% |
| **Auto-Pause CPU Usage** | **0.0%** (Full thread hibernation) | 0.5% – 2.0% |
| **Battery Conservation** | **Auto-pause on DC / Saver / Low Battery or Adaptive FPS** | None / Always active |
| **Binary Distribution** | **~1.8 MB standalone executable (Portable ZIP bundle)** | 50–200 MB (Requires DLLs / runtimes) |
| **External Dependencies** | **None (Zero third-party DLLs)** | Requires `libmpv`, `ffmpeg`, or Chromium |

---

## ✨ Features

- **🔋 Power & Session Awareness (v1.8.0)**
  - **Laptop Battery Intelligence**: Reads `GetSystemPowerStatus` to auto-pause on DC battery power, during Windows Battery Saver mode, or when battery drops below a configurable threshold (Off / 10% / 20% / 30%).
  - **Adaptive Battery FPS**: Dynamically locks rendering to 30 FPS or 20 FPS while on battery power to conserve laptop energy without sacrificing desktop responsiveness.
  - **Session Lifecycle Awareness**: Seamlessly hibernates rendering during Workstation Lock (`WM_WTSSESSION_CHANGE`), Remote Desktop connections, Display Power-Off (`GUID_CONSOLE_DISPLAY_STATE`), and System Sleep/Hibernate (`PBT_APMSUSPEND` / `PBT_APMRESUMEAUTOMATIC`).
  - **Exclusive Fullscreen Gaming**: Queries `SHQueryUserNotificationState` (`QUNS_BUSY` / `QUNS_RUNNING_D3D_FULL_SCREEN`) to pause only the monitor running the exclusive fullscreen application.
  - **Windows Accessibility Respect**: Honors the Windows "Show animations in Windows" setting (`SPI_GETCLIENTAREAANIMATION`) to automatically suspend motion when animations are globally disabled.

- **⚡ Event-Driven WinEvent Hook (v1.8.0)**
  - Replaces periodic 1 Hz polling loops with native Windows event hooks (`EVENT_SYSTEM_FOREGROUND`, `EVENT_OBJECT_LOCATIONCHANGE`, `EVENT_SYSTEM_MINIMIZESTART`, etc.).
  - Employs a 40 ms settle coalescer to achieve sub-100 ms pause latency (p95 ≤ 150 ms PRD target exceeded) while eliminating CPU wakeups when desktop windows are static.

- **🎯 Unified Orthogonal Pause Model (`PauseReason` Bitflags)**
  - Multi-cause bitflag state machine tracks reasons independently (`MANUAL`, `COVERED`, `BATTERY_SAVER`, `ON_BATTERY`, `BATTERY_LOW`, `SESSION_LOCKED`, `REMOTE_SESSION`, `DISPLAY_OFF`, `SYSTEM_SUSPENDED`, `EXCLUSIVE_FULLSCREEN`, `REDUCE_MOTION`).
  - Independent pause reasons cannot cancel or overwrite each other (e.g., unlocking the workstation while covered keeps the wallpaper paused).

- **🚀 True Zero-Copy D3D11 Hardware Pipeline**
  - Bitstreams are parsed by a lightweight pure-Rust MP4 demuxer and fed directly into the GPU via Windows Media Foundation Transform (`IMFTransform`).
  - Decoded NV12 surfaces remain entirely inside GPU memory and are mapped directly into a custom HLSL pixel shader for color conversion and aspect-ratio scaling.

- **🖱️ Native Desktop Interaction & Mouse Pass-Through**
  - Child rendering surface intercepts `WM_NCHITTEST` and returns `HTTRANSPARENT` (`-1`).
  - Desktop icons, marquee selections, double-clicks, and context menus pass straight through to `SHELLDLL_DefView` without focus stealing or loading cursor stalls.

- **🎮 Smart Auto-Pause for Gaming & Productivity**
  - Background monitor automatically detects fullscreen games or maximized windows.
  - Instantly hibernates playback on a synchronization `Condvar`, invokes `IDXGIDevice3::Trim` to release GPU driver caches, and trims the process working set (0% CPU, < 8 MB RAM).
  - Resumes instantly and seamlessly when returning to the desktop.

- **🔄 Infinite Seamless Monotonic Looping**
  - In-memory frame index table calculates presentation timestamps (PTS) monotonically forward across loop cycles.
  - Zero timeline seeking, zero audio pops, and zero black-frame stutter during continuous playback loops.

- **💎 Multi-Display, Dynamic DPI & Shared Decoding**
  - Full Per-Monitor V2 DPI awareness with dynamic `WM_DISPLAYCHANGE`, `WM_DPICHANGED`, hot-plug, and `explorer.exe` (`TaskbarCreated`) recovery without restarting.
  - Supports **Per-Monitor** (independent videos & settings per screen), **Clone**, and seamless **Span** across virtual desktop monitors (including negative coordinates), with shared decoding (`SharedVideoSession`) when displays share the same video.
  - Per-monitor Smart Auto-Pause suspends only covered displays while keeping uncovered displays running.

- **🔇 Silent Windows Subsystem Execution**
  - Native `#![windows_subsystem = "windows"]` configuration ensures no terminal or console window flashes during launch or startup.

- **🖱️ System Tray Control Center**
  - Non-blocking notification area tray menu with instant controls:
    - **Upload New Wallpaper...** (native Win32 file picker)
    - **Select Active Wallpaper** (switch wallpapers on-the-fly with live indicators)
    - **Pause / Resume Wallpaper**
    - **Playback Speed** (0.5x, 1.0x, 1.5x)
    - **Screen Fit Mode** (Cover, Fill, Contain)
    - **Brightness Control** (100%, 75%, 50%)
    - **Target FPS Profiles** (Unlimited, 60 FPS, 30 FPS, 20 FPS, 15 FPS)
    - **Power & Battery Submenu**:
      - Pause on Battery Saver
      - Pause on Battery Power
      - Low Battery Pause (Off / 10% / 20% / 30%)
      - Battery FPS Limit (Uncapped / 30 FPS / 20 FPS)
      - Pause on Lock / Sleep
      - Exclusive Fullscreen Pause
      - Follow Windows Animation Settings
    - **Start with Windows** (clean registry persistence via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`)
    - **Check for Updates...** (non-blocking GitHub release notifier)
    - **Uninstall ArcPaper** (cleanly deregisters registry entries and cleans up)

---

## 🛠️ Architecture & Tech Stack

```mermaid
flowchart LR
    MP4[".mp4 Video File"] --> Demuxer["Pure-Rust Demuxer\n(moov / stbl Box Parser)"]
    Demuxer --> AnnexB["Annex-B Bitstream Packets\n(Monotonic PTS Loop Controller)"]
    AnnexB --> Decoder["GPU Hardware MFT Decoder\n(IMFTransform + IMFDXGIDeviceManager)"]
    Decoder --> NV12["D3D11 NV12 Texture2D\n(Direct VRAM Allocation)"]
    NV12 --> Shader["HLSL Pixel Shader\n(NV12 to BGRA + Aspect Ratio)"]
    Shader --> SwapChain["Flip-Model SwapChain\n(DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL)"]
    SwapChain --> WorkerW["Desktop Shell Window\n(WorkerW / Progman)"]
```

| Component | Technology | Description |
|---|---|---|
| **Language** | [Rust](https://www.rust-lang.org/) (2024 edition) | Memory-safe, high-performance systems language |
| **Video Decoding** | Windows Media Foundation (`IMFTransform`) | Hardware video decoding (H.264 / HEVC) via GPU chip |
| **Graphics & Presentation** | Direct3D 11 & DXGI | Flip-model swapchain presenting directly onto desktop surfaces |
| **Desktop Integration** | Win32 API (`windows` crate) | Direct attachment to `WorkerW` / `Progman` with `HTTRANSPARENT` |
| **System Event Hooks** | Win32 WinEvent & Power Notifications | Low-latency window events, battery status & session changes |
| **System Tray UI** | [tray-icon](https://crates.io/crates/tray-icon) & [muda](https://crates.io/crates/muda) | Native, responsive notification tray interface |
| **Configuration** | [serde](https://crates.io/crates/serde) & `serde_json` | Schema v3 JSON settings stored locally |

---

## 📥 Installation & Usage

### Portable Distribution Archive (Recommended)

1. Download the latest release package: [`ArcPaper-v1.8.0-win64.zip`](https://github.com/ArtixForLiving/ArcPaper/releases/latest).
2. Extract the ZIP archive to your preferred directory (e.g., `C:\Tools\ArcPaper` or `D:\ArcPaper`).
3. Run `arc_paper.exe`.
4. Right-click the **ArcPaper** icon in your Windows System Tray:
   - Click **"Upload New Wallpaper..."** to select any `.mp4` video.
   - Or choose from the preloaded sample wallpapers in the `SavedWallpapers` folder.
   - Configure **Power & Battery** options to fit your workflow.
   - Check **"Start with Windows"** if you want ArcPaper to launch automatically on login.

> **Tip:** You can drop any additional `.mp4` video files directly into the `SavedWallpapers` folder next to `arc_paper.exe` to access them instantly from the tray menu!

---

## 🏗️ Building from Source

### Prerequisites
- [Rust toolchain](https://rustup.rs/) (edition 2024 / Rust 1.85+)
- Windows 10 or 11 (64-bit)
- No external C-libraries, runtimes, or DLL downloads required!

### Compilation
```powershell
# Clone the repository
git clone https://github.com/ArtixForLiving/ArcPaper.git
cd ArcPaper

# Compile the optimized release binary
cargo build --release

# Run the test suite (140+ unit and integration tests)
cargo test --verbose
```

The compiled standalone executable will be located at:
`target/release/arc_paper.exe` (~1.8 MB).

---

## 📄 License

This project is licensed under the [MIT License](LICENSE.md).
