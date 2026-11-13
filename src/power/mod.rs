//! Power and session state monitoring for intelligent wallpaper pause and resume (FR-302, FR-303).
//!
//! Provides battery power status tracking, Windows Terminal Services (WTS) session
//! monitoring, console display state awareness, and mockable [`SystemProbe`] implementations.

pub mod battery;
pub mod session;
pub mod system_probe;

pub use battery::{BatteryPolicy, evaluate_battery_pause};
pub use session::{SessionState, evaluate_session_pause};
pub use system_probe::{
    ConsoleDisplayState, MockSystemProbe, PowerStatus, SystemProbe, WindowsSystemProbe,
};

#[cfg(windows)]
pub use windows::Win32::System::SystemServices::{
    GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING, GUID_CONSOLE_DISPLAY_STATE,
    GUID_POWER_SAVING_STATUS as GUID_ENERGY_SAVER_STATUS,
};
