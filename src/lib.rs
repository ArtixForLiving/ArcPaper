//! ArcPaper core library exposing desktop integration, media playback engine, and application helpers.

pub mod app;
pub mod arc_player;
pub mod display;
pub mod power;
#[cfg(windows)]
pub mod workerw;

pub use app::{ArcPaperEventHost, HostEvent, PauseReason, winevent};
pub use power::{
    BatteryPolicy, ConsoleDisplayState, MockSystemProbe, PowerStatus, SessionState, SystemProbe,
    WindowsSystemProbe, evaluate_battery_pause, evaluate_session_pause,
};
