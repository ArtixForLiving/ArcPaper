//! ArcPaper core library exposing desktop integration, media playback engine, and application helpers.

pub mod app;
pub mod arc_player;
#[cfg(windows)]
pub mod workerw;
