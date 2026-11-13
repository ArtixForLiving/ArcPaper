//! Application-layer modules separated from the main GUI entry point.

pub mod autopause_monitor;
#[cfg(windows)]
pub mod autostart;
pub mod config;
pub mod event_host;
pub mod log;
pub mod paths;
pub mod pause;
pub use event_host::{ArcPaperEventHost, HostEvent};
pub use pause::PauseReason;
#[cfg(windows)]
pub mod tray;
pub mod uninstall;
pub mod updater;
pub mod winevent;
