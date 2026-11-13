//! Battery policy and power decision table evaluation (FR-302).
//!
//! Provides configurable battery management policies for laptops and battery-powered devices:
//! - [`BatteryPolicy::AlwaysPlay`]: Continuous playback regardless of power source.
//! - [`BatteryPolicy::PauseOnBatterySaver`]: Pauses only when Windows Battery Saver / Energy Saver mode is engaged (default).
//! - [`BatteryPolicy::PauseOnBattery`]: Pauses whenever operating on DC battery power.

use super::system_probe::PowerStatus;
use crate::app::PauseReason;
use serde::{Deserialize, Serialize};

/// User-selectable battery management policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BatteryPolicy {
    /// Never pause playback due to battery or power saver state.
    #[serde(alias = "always_play", alias = "always-play")]
    AlwaysPlay,
    /// Pause playback when Windows Battery Saver / Energy Saver is active (default).
    #[default]
    #[serde(alias = "pause_on_battery_saver", alias = "pause-on-battery-saver")]
    PauseOnBatterySaver,
    /// Pause playback whenever running on DC battery power.
    #[serde(alias = "pause_on_battery", alias = "pause-on-battery")]
    PauseOnBattery,
}

/// Evaluates whether wallpaper playback should pause given the active [`BatteryPolicy`] and [`PowerStatus`].
///
/// Follows the deterministic decision table:
/// 1. Desktop PCs without a battery (`!status.has_battery`) never pause (`PauseReason::empty()`).
/// 2. Systems plugged into AC power (`status.ac_online`) never pause (`PauseReason::empty()`).
/// 3. If policy is [`BatteryPolicy::AlwaysPlay`], returns `PauseReason::empty()`.
/// 4. If policy is [`BatteryPolicy::PauseOnBatterySaver`], returns `PauseReason::BATTERY_SAVER` if saver is engaged, else `empty()`.
/// 5. If policy is [`BatteryPolicy::PauseOnBattery`], returns `PauseReason::BATTERY`.
#[inline]
pub fn evaluate_battery_pause(policy: BatteryPolicy, status: &PowerStatus) -> PauseReason {
    if !status.has_battery || status.ac_online {
        return PauseReason::empty();
    }

    match policy {
        BatteryPolicy::AlwaysPlay => PauseReason::empty(),
        BatteryPolicy::PauseOnBatterySaver => {
            if status.battery_saver {
                PauseReason::BATTERY_SAVER
            } else {
                PauseReason::empty()
            }
        }
        BatteryPolicy::PauseOnBattery => PauseReason::BATTERY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_battery_policy_default_is_pause_on_battery_saver() {
        assert_eq!(BatteryPolicy::default(), BatteryPolicy::PauseOnBatterySaver);
    }

    #[test]
    fn test_desktop_without_battery_never_pauses_ac_online() {
        let status = PowerStatus {
            ac_online: true,
            battery_saver: false,
            battery_percent: None,
            has_battery: false,
        };
        for policy in [
            BatteryPolicy::AlwaysPlay,
            BatteryPolicy::PauseOnBatterySaver,
            BatteryPolicy::PauseOnBattery,
        ] {
            assert_eq!(
                evaluate_battery_pause(policy, &status),
                PauseReason::empty(),
                "Desktop on AC with policy {:?} must not pause",
                policy
            );
        }
    }

    #[test]
    fn test_desktop_without_battery_never_pauses_even_if_reported_offline() {
        let status = PowerStatus {
            ac_online: false,
            battery_saver: true,
            battery_percent: None,
            has_battery: false,
        };
        for policy in [
            BatteryPolicy::AlwaysPlay,
            BatteryPolicy::PauseOnBatterySaver,
            BatteryPolicy::PauseOnBattery,
        ] {
            assert_eq!(
                evaluate_battery_pause(policy, &status),
                PauseReason::empty(),
                "Desktop without battery must never pause regardless of policy {:?}",
                policy
            );
        }
    }

    #[test]
    fn test_laptop_on_ac_never_pauses_saver_off() {
        let status = PowerStatus {
            ac_online: true,
            battery_saver: false,
            battery_percent: Some(100),
            has_battery: true,
        };
        for policy in [
            BatteryPolicy::AlwaysPlay,
            BatteryPolicy::PauseOnBatterySaver,
            BatteryPolicy::PauseOnBattery,
        ] {
            assert_eq!(
                evaluate_battery_pause(policy, &status),
                PauseReason::empty(),
                "Laptop plugged into AC with policy {:?} must not pause",
                policy
            );
        }
    }

    #[test]
    fn test_laptop_on_ac_never_pauses_even_if_saver_reported_active() {
        let status = PowerStatus {
            ac_online: true,
            battery_saver: true,
            battery_percent: Some(80),
            has_battery: true,
        };
        for policy in [
            BatteryPolicy::AlwaysPlay,
            BatteryPolicy::PauseOnBatterySaver,
            BatteryPolicy::PauseOnBattery,
        ] {
            assert_eq!(
                evaluate_battery_pause(policy, &status),
                PauseReason::empty(),
                "Laptop plugged into AC with policy {:?} must not pause even with saver active",
                policy
            );
        }
    }

    #[test]
    fn test_laptop_on_battery_always_play_policy() {
        let status_normal = PowerStatus {
            ac_online: false,
            battery_saver: false,
            battery_percent: Some(75),
            has_battery: true,
        };
        let status_saver = PowerStatus {
            ac_online: false,
            battery_saver: true,
            battery_percent: Some(15),
            has_battery: true,
        };

        assert_eq!(
            evaluate_battery_pause(BatteryPolicy::AlwaysPlay, &status_normal),
            PauseReason::empty()
        );
        assert_eq!(
            evaluate_battery_pause(BatteryPolicy::AlwaysPlay, &status_saver),
            PauseReason::empty()
        );
    }

    #[test]
    fn test_laptop_on_battery_pause_on_battery_saver_policy() {
        let status_saver_off = PowerStatus {
            ac_online: false,
            battery_saver: false,
            battery_percent: Some(50),
            has_battery: true,
        };
        let status_saver_on = PowerStatus {
            ac_online: false,
            battery_saver: true,
            battery_percent: Some(19),
            has_battery: true,
        };

        assert_eq!(
            evaluate_battery_pause(BatteryPolicy::PauseOnBatterySaver, &status_saver_off),
            PauseReason::empty(),
            "Should not pause when battery saver is inactive"
        );
        assert_eq!(
            evaluate_battery_pause(BatteryPolicy::PauseOnBatterySaver, &status_saver_on),
            PauseReason::BATTERY_SAVER,
            "Must pause with BATTERY_SAVER when battery saver is active"
        );
    }

    #[test]
    fn test_laptop_on_battery_pause_on_battery_policy() {
        let status_saver_off = PowerStatus {
            ac_online: false,
            battery_saver: false,
            battery_percent: Some(95),
            has_battery: true,
        };
        let status_saver_on = PowerStatus {
            ac_online: false,
            battery_saver: true,
            battery_percent: Some(10),
            has_battery: true,
        };

        assert_eq!(
            evaluate_battery_pause(BatteryPolicy::PauseOnBattery, &status_saver_off),
            PauseReason::BATTERY,
            "Must pause with BATTERY whenever running on DC"
        );
        assert_eq!(
            evaluate_battery_pause(BatteryPolicy::PauseOnBattery, &status_saver_on),
            PauseReason::BATTERY,
            "Must pause with BATTERY when running on DC even with saver active"
        );
    }

    #[test]
    fn test_exhaustive_battery_decision_matrix() {
        let policies = [
            BatteryPolicy::AlwaysPlay,
            BatteryPolicy::PauseOnBatterySaver,
            BatteryPolicy::PauseOnBattery,
        ];
        let ac_states = [true, false];
        let has_battery_states = [true, false];
        let saver_states = [true, false];

        for &policy in &policies {
            for &ac in &ac_states {
                for &has_battery in &has_battery_states {
                    for &saver in &saver_states {
                        let status = PowerStatus {
                            ac_online: ac,
                            battery_saver: saver,
                            battery_percent: if has_battery { Some(50) } else { None },
                            has_battery,
                        };

                        let result = evaluate_battery_pause(policy, &status);

                        if !has_battery || ac {
                            assert_eq!(result, PauseReason::empty());
                        } else {
                            match policy {
                                BatteryPolicy::AlwaysPlay => {
                                    assert_eq!(result, PauseReason::empty())
                                }
                                BatteryPolicy::PauseOnBatterySaver => {
                                    if saver {
                                        assert_eq!(result, PauseReason::BATTERY_SAVER);
                                    } else {
                                        assert_eq!(result, PauseReason::empty());
                                    }
                                }
                                BatteryPolicy::PauseOnBattery => {
                                    assert_eq!(result, PauseReason::BATTERY);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
