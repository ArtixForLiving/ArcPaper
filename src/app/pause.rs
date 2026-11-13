//! Unified pause reason model and truth matrix resolution.
//!
//! Separates per-monitor pause reasons (coverage, exclusive fullscreen) from global
//! system-level pause reasons (manual pause, battery, lock, RDP, display off, suspend).

use bitflags::bitflags;

bitflags! {
    /// Comprehensive bitflags representing all possible causes for pausing wallpaper rendering.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PauseReason: u32 {
        /// User manually paused wallpaper playback via tray menu or hotkey.
        const MANUAL               = 1 << 0;
        /// Monitor workspace is obscured by a maximized, full-screen, or covering window.
        const COVERED              = 1 << 1;
        /// System is operating on DC battery power.
        const BATTERY              = 1 << 2;
        /// Windows Energy Saver / Battery Saver mode is active.
        const BATTERY_SAVER        = 1 << 3;
        /// Current Windows user session is locked (Win+L / screensaver).
        const SESSION_LOCKED       = 1 << 4;
        /// Active session is running over Remote Desktop (RDP / Terminal Services).
        const REMOTE_SESSION       = 1 << 5;
        /// Display / monitor has been powered down by Windows power management.
        const DISPLAY_OFF          = 1 << 6;
        /// An exclusive fullscreen game or 3D graphics application is focused.
        const EXCLUSIVE_FULLSCREEN = 1 << 7;
        /// Windows accessibility setting "Turn off unnecessary animations" is enabled.
        const REDUCE_MOTION        = 1 << 8;
        /// Windows system is entering sleep or suspended power state (S3/Modern Standby).
        const SUSPENDED            = 1 << 9;
    }
}

impl PauseReason {
    /// Bitmask isolating all per-monitor pause reasons.
    pub const PER_MONITOR_MASK: Self = Self::COVERED.union(Self::EXCLUSIVE_FULLSCREEN);

    /// Bitmask isolating all global system pause reasons.
    pub const GLOBAL_MASK: Self = Self::MANUAL
        .union(Self::BATTERY)
        .union(Self::BATTERY_SAVER)
        .union(Self::SESSION_LOCKED)
        .union(Self::REMOTE_SESSION)
        .union(Self::DISPLAY_OFF)
        .union(Self::REDUCE_MOTION)
        .union(Self::SUSPENDED);

    /// Returns `true` if this reason contains any per-monitor scoped flags.
    #[inline]
    pub fn is_per_monitor(&self) -> bool {
        self.intersects(Self::PER_MONITOR_MASK)
    }

    /// Returns `true` if this reason contains any global scoped flags.
    #[inline]
    pub fn is_global(&self) -> bool {
        self.intersects(Self::GLOBAL_MASK)
    }

    /// Returns a human-readable description for the dominant pause reason,
    /// prioritized according to severity and user relevance.
    pub fn dominant_reason_label(&self) -> Option<&'static str> {
        if self.contains(Self::SUSPENDED) {
            Some("System Suspended")
        } else if self.contains(Self::DISPLAY_OFF) {
            Some("Display Turned Off")
        } else if self.contains(Self::SESSION_LOCKED) {
            Some("Session Locked")
        } else if self.contains(Self::REMOTE_SESSION) {
            Some("Remote Desktop Active")
        } else if self.contains(Self::BATTERY_SAVER) {
            Some("Battery Saver Active")
        } else if self.contains(Self::BATTERY) {
            Some("Running on Battery")
        } else if self.contains(Self::MANUAL) {
            Some("Manually Paused")
        } else if self.contains(Self::EXCLUSIVE_FULLSCREEN) {
            Some("Exclusive Fullscreen App")
        } else if self.contains(Self::COVERED) {
            Some("Covered / Maximized Window")
        } else if self.contains(Self::REDUCE_MOTION) {
            Some("Reduce Motion Active")
        } else {
            None
        }
    }
}

/// Constant alias exposing `PER_MONITOR_MASK` at the module level.
pub const PER_MONITOR_MASK: PauseReason = PauseReason::PER_MONITOR_MASK;

/// Constant alias exposing `GLOBAL_MASK` at the module level.
pub const GLOBAL_MASK: PauseReason = PauseReason::GLOBAL_MASK;

/// Determines whether a monitor must be paused given global and monitor-specific pause reasons.
#[inline]
pub fn resolve_effective_monitor_pause(
    global_reasons: PauseReason,
    monitor_reasons: PauseReason,
) -> bool {
    !(global_reasons | monitor_reasons).is_empty()
}

/// Evaluates whether the entire wallpaper system should enter global hibernation.
///
/// Returns `true` if there is at least one active monitor and all monitors are effectively paused.
#[inline]
pub fn should_enter_global_hibernation(all_monitor_effective_pauses: &[bool]) -> bool {
    !all_monitor_effective_pauses.is_empty()
        && all_monitor_effective_pauses.iter().all(|&paused| paused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pause_reason_default_is_empty() {
        let default_reason = PauseReason::default();
        assert!(default_reason.is_empty());
        assert_eq!(default_reason.bits(), 0);
    }

    #[test]
    fn test_scope_masks_are_disjoint_and_exhaustive() {
        // PER_MONITOR_MASK and GLOBAL_MASK must share no bits
        assert!(PER_MONITOR_MASK.intersection(GLOBAL_MASK).is_empty());

        // Their combination must cover all defined flags
        let combined = PER_MONITOR_MASK | GLOBAL_MASK;
        assert_eq!(combined, PauseReason::all());
    }

    #[test]
    fn test_global_vs_per_monitor_classification() {
        // Per-monitor reasons
        assert!(PauseReason::COVERED.is_per_monitor());
        assert!(!PauseReason::COVERED.is_global());

        assert!(PauseReason::EXCLUSIVE_FULLSCREEN.is_per_monitor());
        assert!(!PauseReason::EXCLUSIVE_FULLSCREEN.is_global());

        // Global reasons
        let global_flags = [
            PauseReason::MANUAL,
            PauseReason::BATTERY,
            PauseReason::BATTERY_SAVER,
            PauseReason::SESSION_LOCKED,
            PauseReason::REMOTE_SESSION,
            PauseReason::DISPLAY_OFF,
            PauseReason::REDUCE_MOTION,
            PauseReason::SUSPENDED,
        ];

        for flag in global_flags {
            assert!(
                flag.is_global(),
                "{:?} should be classified as global",
                flag
            );
            assert!(
                !flag.is_per_monitor(),
                "{:?} should not be classified as per-monitor",
                flag
            );
        }

        // Empty reason has neither
        let empty = PauseReason::empty();
        assert!(!empty.is_per_monitor());
        assert!(!empty.is_global());

        // Mixed reasons have both
        let mixed = PauseReason::COVERED | PauseReason::BATTERY;
        assert!(mixed.is_per_monitor());
        assert!(mixed.is_global());
    }

    #[test]
    fn test_resolve_effective_monitor_pause_empty() {
        assert!(!resolve_effective_monitor_pause(
            PauseReason::empty(),
            PauseReason::empty()
        ));
    }

    #[test]
    fn test_resolve_effective_monitor_pause_global_only() {
        let global_flags = [
            PauseReason::MANUAL,
            PauseReason::BATTERY,
            PauseReason::BATTERY_SAVER,
            PauseReason::SESSION_LOCKED,
            PauseReason::REMOTE_SESSION,
            PauseReason::DISPLAY_OFF,
            PauseReason::REDUCE_MOTION,
            PauseReason::SUSPENDED,
        ];

        for flag in global_flags {
            assert!(
                resolve_effective_monitor_pause(flag, PauseReason::empty()),
                "Expected effective pause when global reason {:?} is active",
                flag
            );
        }
    }

    #[test]
    fn test_resolve_effective_monitor_pause_monitor_only() {
        let monitor_flags = [PauseReason::COVERED, PauseReason::EXCLUSIVE_FULLSCREEN];

        for flag in monitor_flags {
            assert!(
                resolve_effective_monitor_pause(PauseReason::empty(), flag),
                "Expected effective pause when monitor reason {:?} is active",
                flag
            );
        }
    }

    #[test]
    fn test_truth_matrix_all_pairs() {
        let all_reasons = [
            PauseReason::MANUAL,
            PauseReason::COVERED,
            PauseReason::BATTERY,
            PauseReason::BATTERY_SAVER,
            PauseReason::SESSION_LOCKED,
            PauseReason::REMOTE_SESSION,
            PauseReason::DISPLAY_OFF,
            PauseReason::EXCLUSIVE_FULLSCREEN,
            PauseReason::REDUCE_MOTION,
            PauseReason::SUSPENDED,
        ];

        for g in all_reasons {
            for m in all_reasons {
                assert!(
                    resolve_effective_monitor_pause(g, m),
                    "Combination {:?} + {:?} must resolve to paused",
                    g,
                    m
                );
            }
        }
    }

    #[test]
    fn test_manual_unpause_retains_pause_when_covered() {
        // Initially both manual pause and covered are active
        let mut global = PauseReason::MANUAL;
        let monitor = PauseReason::COVERED;
        assert!(resolve_effective_monitor_pause(global, monitor));

        // User unpauses manually -> global becomes empty, but monitor remains COVERED
        global.remove(PauseReason::MANUAL);
        assert!(global.is_empty());
        assert!(
            resolve_effective_monitor_pause(global, monitor),
            "Monitor must remain paused because it is still COVERED"
        );
    }

    #[test]
    fn test_manual_unpause_retains_pause_when_battery_saver_active() {
        // User pauses manually while Battery Saver is active
        let mut global = PauseReason::MANUAL | PauseReason::BATTERY_SAVER;
        let monitor = PauseReason::empty();
        assert!(resolve_effective_monitor_pause(global, monitor));

        // User unpauses manually
        global.remove(PauseReason::MANUAL);
        assert_eq!(global, PauseReason::BATTERY_SAVER);
        assert!(
            resolve_effective_monitor_pause(global, monitor),
            "Wallpaper must remain paused because BATTERY_SAVER is active"
        );
    }

    #[test]
    fn test_manual_unpause_retains_pause_when_session_locked() {
        // System locked while manually paused
        let mut global = PauseReason::MANUAL | PauseReason::SESSION_LOCKED;
        let monitor = PauseReason::empty();
        assert!(resolve_effective_monitor_pause(global, monitor));

        // User unpauses manually
        global.remove(PauseReason::MANUAL);
        assert_eq!(global, PauseReason::SESSION_LOCKED);
        assert!(
            resolve_effective_monitor_pause(global, monitor),
            "Wallpaper must remain paused because SESSION_LOCKED is active"
        );
    }

    #[test]
    fn test_multi_monitor_divergent_coverage_with_global_reason() {
        let mut global = PauseReason::empty();
        let mon1_reasons = PauseReason::COVERED;
        let mon2_reasons = PauseReason::empty();

        // Without global reason: Mon 1 paused, Mon 2 playing
        assert!(resolve_effective_monitor_pause(global, mon1_reasons));
        assert!(!resolve_effective_monitor_pause(global, mon2_reasons));
        assert!(
            !should_enter_global_hibernation(&[true, false]),
            "Partial coverage should not trigger global hibernation"
        );

        // Global BATTERY reason kicks in
        global.insert(PauseReason::BATTERY);
        let mon1_paused = resolve_effective_monitor_pause(global, mon1_reasons);
        let mon2_paused = resolve_effective_monitor_pause(global, mon2_reasons);
        assert!(mon1_paused);
        assert!(mon2_paused);
        assert!(
            should_enter_global_hibernation(&[mon1_paused, mon2_paused]),
            "Both paused triggers global hibernation"
        );

        // Battery plugged back in
        global.remove(PauseReason::BATTERY);
        let mon1_paused_after = resolve_effective_monitor_pause(global, mon1_reasons);
        let mon2_paused_after = resolve_effective_monitor_pause(global, mon2_reasons);
        assert!(mon1_paused_after);
        assert!(!mon2_paused_after);
        assert!(
            !should_enter_global_hibernation(&[mon1_paused_after, mon2_paused_after]),
            "Mon 2 resuming exits global hibernation"
        );
    }

    #[test]
    fn test_should_enter_global_hibernation() {
        assert!(!should_enter_global_hibernation(&[]));
        assert!(!should_enter_global_hibernation(&[false]));
        assert!(should_enter_global_hibernation(&[true]));
        assert!(!should_enter_global_hibernation(&[true, false]));
        assert!(!should_enter_global_hibernation(&[false, true]));
        assert!(should_enter_global_hibernation(&[true, true]));
        assert!(!should_enter_global_hibernation(&[false, false]));
        assert!(should_enter_global_hibernation(&[true, true, true]));
        assert!(!should_enter_global_hibernation(&[true, false, true]));
    }

    #[test]
    fn test_dominant_reason_label_priority_hierarchy() {
        assert_eq!(PauseReason::empty().dominant_reason_label(), None);
        assert_eq!(
            PauseReason::SUSPENDED.dominant_reason_label(),
            Some("System Suspended")
        );
        assert_eq!(
            PauseReason::DISPLAY_OFF.dominant_reason_label(),
            Some("Display Turned Off")
        );
        assert_eq!(
            PauseReason::SESSION_LOCKED.dominant_reason_label(),
            Some("Session Locked")
        );
        assert_eq!(
            PauseReason::REMOTE_SESSION.dominant_reason_label(),
            Some("Remote Desktop Active")
        );
        assert_eq!(
            PauseReason::BATTERY_SAVER.dominant_reason_label(),
            Some("Battery Saver Active")
        );
        assert_eq!(
            PauseReason::BATTERY.dominant_reason_label(),
            Some("Running on Battery")
        );
        assert_eq!(
            PauseReason::MANUAL.dominant_reason_label(),
            Some("Manually Paused")
        );
        assert_eq!(
            PauseReason::EXCLUSIVE_FULLSCREEN.dominant_reason_label(),
            Some("Exclusive Fullscreen App")
        );
        assert_eq!(
            PauseReason::COVERED.dominant_reason_label(),
            Some("Covered / Maximized Window")
        );
        assert_eq!(
            PauseReason::REDUCE_MOTION.dominant_reason_label(),
            Some("Reduce Motion Active")
        );
    }

    #[test]
    fn test_dominant_reason_label_precedence_combinations() {
        // SUSPENDED beats everything
        let s = PauseReason::SUSPENDED | PauseReason::DISPLAY_OFF | PauseReason::MANUAL;
        assert_eq!(s.dominant_reason_label(), Some("System Suspended"));

        // DISPLAY_OFF beats SESSION_LOCKED and lower
        let d = PauseReason::DISPLAY_OFF | PauseReason::SESSION_LOCKED | PauseReason::COVERED;
        assert_eq!(d.dominant_reason_label(), Some("Display Turned Off"));

        // SESSION_LOCKED beats REMOTE_SESSION
        let l = PauseReason::SESSION_LOCKED | PauseReason::REMOTE_SESSION;
        assert_eq!(l.dominant_reason_label(), Some("Session Locked"));

        // REMOTE_SESSION beats BATTERY_SAVER
        let r = PauseReason::REMOTE_SESSION | PauseReason::BATTERY_SAVER;
        assert_eq!(r.dominant_reason_label(), Some("Remote Desktop Active"));

        // BATTERY_SAVER beats BATTERY
        let bs = PauseReason::BATTERY_SAVER | PauseReason::BATTERY;
        assert_eq!(bs.dominant_reason_label(), Some("Battery Saver Active"));

        // BATTERY beats MANUAL
        let b = PauseReason::BATTERY | PauseReason::MANUAL;
        assert_eq!(b.dominant_reason_label(), Some("Running on Battery"));

        // MANUAL beats EXCLUSIVE_FULLSCREEN
        let m = PauseReason::MANUAL | PauseReason::EXCLUSIVE_FULLSCREEN;
        assert_eq!(m.dominant_reason_label(), Some("Manually Paused"));

        // EXCLUSIVE_FULLSCREEN beats COVERED
        let x = PauseReason::EXCLUSIVE_FULLSCREEN | PauseReason::COVERED;
        assert_eq!(x.dominant_reason_label(), Some("Exclusive Fullscreen App"));

        // COVERED beats REDUCE_MOTION
        let c = PauseReason::COVERED | PauseReason::REDUCE_MOTION;
        assert_eq!(
            c.dominant_reason_label(),
            Some("Covered / Maximized Window")
        );
    }
}
