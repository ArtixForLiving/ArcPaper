//! ArcPaper core library exposing desktop integration, media playback engine, and application helpers.

pub mod app;
pub mod arc_player;
pub mod display;
#[cfg(windows)]
pub mod workerw;

pub use app::PauseReason;
