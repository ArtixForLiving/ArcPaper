//! Hardware and OS power status probe abstraction.
//!
//! Exposes the [`SystemProbe`] trait to enable 100% offline, deterministic unit testing
//! and CI verification of power management decisions without physical battery hardware.

/// Represents the instantaneous power state of the host machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerStatus {
    /// True if the system is connected to an external AC power source.
    pub ac_online: bool,
    /// True if Windows Battery Saver / Energy Saver mode is currently active.
    pub battery_saver: bool,
    /// Remaining battery charge percentage (0-100), or `None` if unknown or absent.
    pub battery_percent: Option<u8>,
    /// True if the hardware reports an installed system battery (e.g. laptop, tablet).
    pub has_battery: bool,
}

impl Default for PowerStatus {
    fn default() -> Self {
        Self {
            ac_online: true,
            battery_saver: false,
            battery_percent: None,
            has_battery: false,
        }
    }
}

/// The power and activity state of the physical console display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleDisplayState {
    /// Console display is powered on and active.
    #[default]
    On,
    /// Console display is dimmed due to user inactivity.
    Dimmed,
    /// Console display has entered low-power or off state.
    Off,
}

/// Abstraction layer over Windows power and session APIs.
pub trait SystemProbe: Send + Sync {
    /// Returns the current hardware power status (AC/DC, battery saver, percent).
    fn get_power_status(&self) -> PowerStatus;

    /// Returns `true` if the current user session is running over Remote Desktop (RDP).
    fn is_remote_session(&self) -> bool;

    /// Returns the current console display state (On, Dimmed, or Off).
    fn get_console_display_state(&self) -> ConsoleDisplayState;

    /// Returns the current Windows Shell User Notification State (QUNS), or `None` if query failed.
    fn query_quns(&self) -> Option<i32>;

    /// Returns `true` if Windows client area animations are enabled.
    fn is_animation_enabled(&self) -> bool;
}

/// Production implementation of [`SystemProbe`] calling Win32 platform APIs.
#[derive(Debug, Default, Clone)]
pub struct WindowsSystemProbe;

impl WindowsSystemProbe {
    /// Creates a new Win32 system probe.
    pub fn new() -> Self {
        Self
    }
}

impl SystemProbe for WindowsSystemProbe {
    fn get_power_status(&self) -> PowerStatus {
        #[cfg(windows)]
        {
            use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
            let mut sps = SYSTEM_POWER_STATUS::default();
            if unsafe { GetSystemPowerStatus(&mut sps) }.is_ok() {
                let ac_online = sps.ACLineStatus == 1;
                // BatteryFlag bit 7 (128) indicates no system battery.
                // 255 indicates an unknown battery status.
                let has_battery = (sps.BatteryFlag & 128) == 0
                    && (sps.BatteryFlag != 255 || sps.BatteryLifePercent <= 100);
                let battery_saver = sps.SystemStatusFlag == 1;
                let battery_percent = if sps.BatteryLifePercent <= 100 {
                    Some(sps.BatteryLifePercent)
                } else {
                    None
                };
                return PowerStatus {
                    ac_online,
                    battery_saver,
                    battery_percent,
                    has_battery,
                };
            }
        }
        PowerStatus::default()
    }

    fn is_remote_session(&self) -> bool {
        #[cfg(windows)]
        {
            use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION};
            unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    fn get_console_display_state(&self) -> ConsoleDisplayState {
        // Without an asynchronous power setting notification event, the active console display is assumed On.
        ConsoleDisplayState::On
    }

    fn query_quns(&self) -> Option<i32> {
        crate::power::quns::query_user_notification_state()
    }

    fn is_animation_enabled(&self) -> bool {
        crate::power::motion::is_client_area_animation_enabled()
    }
}

/// Test mock implementation of [`SystemProbe`] with configurable static states.
#[derive(Debug, Clone, Default)]
pub struct MockSystemProbe {
    pub power_status: PowerStatus,
    pub is_remote: bool,
    pub display_state: ConsoleDisplayState,
    pub quns_state: Option<i32>,
    pub animation_enabled: bool,
}

impl MockSystemProbe {
    /// Creates a mock probe with the specified parameters.
    pub fn new(
        power_status: PowerStatus,
        is_remote: bool,
        display_state: ConsoleDisplayState,
    ) -> Self {
        Self {
            power_status,
            is_remote,
            display_state,
            quns_state: None,
            animation_enabled: true,
        }
    }

    /// Builder method configuring the mock QUNS state.
    pub fn with_quns(mut self, state: Option<i32>) -> Self {
        self.quns_state = state;
        self
    }

    /// Builder method configuring the mock animation enabled status.
    pub fn with_animation(mut self, enabled: bool) -> Self {
        self.animation_enabled = enabled;
        self
    }
}

impl SystemProbe for MockSystemProbe {
    fn get_power_status(&self) -> PowerStatus {
        self.power_status
    }

    fn is_remote_session(&self) -> bool {
        self.is_remote
    }

    fn get_console_display_state(&self) -> ConsoleDisplayState {
        self.display_state
    }

    fn query_quns(&self) -> Option<i32> {
        self.quns_state
    }

    fn is_animation_enabled(&self) -> bool {
        self.animation_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_power_status_default() {
        let status = PowerStatus::default();
        assert!(status.ac_online);
        assert!(!status.battery_saver);
        assert_eq!(status.battery_percent, None);
        assert!(!status.has_battery);
    }

    #[test]
    fn test_console_display_state_default() {
        let state = ConsoleDisplayState::default();
        assert_eq!(state, ConsoleDisplayState::On);
    }

    #[test]
    fn test_mock_system_probe() {
        let status = PowerStatus {
            ac_online: false,
            battery_saver: true,
            battery_percent: Some(42),
            has_battery: true,
        };
        let probe = MockSystemProbe::new(status, true, ConsoleDisplayState::Off)
            .with_quns(Some(3))
            .with_animation(false);
        assert_eq!(probe.get_power_status(), status);
        assert!(probe.is_remote_session());
        assert_eq!(probe.get_console_display_state(), ConsoleDisplayState::Off);
        assert_eq!(probe.query_quns(), Some(3));
        assert!(!probe.is_animation_enabled());
    }

    #[test]
    fn test_windows_system_probe_instantiation() {
        let probe = WindowsSystemProbe::new();
        let _ = probe.get_power_status();
        let _ = probe.is_remote_session();
        assert_eq!(probe.get_console_display_state(), ConsoleDisplayState::On);
        let _ = probe.query_quns();
        let _ = probe.is_animation_enabled();
    }
}
