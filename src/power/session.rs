//! Session, remote desktop, display state, and suspend pause evaluation (FR-303).
//!
//! Evaluates global pause reasons arising from Windows session locking (Win+L / screensaver),
//! Remote Desktop Protocol (RDP) sessions, physical console display sleep, and ACPI system suspend.

use crate::app::PauseReason;

/// Encapsulates the instantaneous session and display state of the application host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionState {
    /// True if the current user session is locked.
    pub is_locked: bool,
    /// True if connected via Remote Desktop (RDP / Terminal Services).
    pub is_remote: bool,
    /// True if the console monitor has powered down / display sleep.
    pub is_display_off: bool,
    /// True if the system is currently suspended / sleeping (S3/Modern Standby).
    pub is_suspended: bool,
}

impl SessionState {
    /// Creates a new `SessionState` with the provided parameters.
    pub fn new(is_locked: bool, is_remote: bool, is_display_off: bool, is_suspended: bool) -> Self {
        Self {
            is_locked,
            is_remote,
            is_display_off,
            is_suspended,
        }
    }

    /// Evaluates the effective [`PauseReason`] for this session state.
    #[inline]
    pub fn evaluate_pause(&self) -> PauseReason {
        evaluate_session_pause(
            self.is_locked,
            self.is_remote,
            self.is_display_off,
            self.is_suspended,
        )
    }
}

/// Pure session state resolver evaluating global pause reasons.
///
/// Combines independent session states into a composite [`PauseReason`] bitmask:
/// - `is_locked` -> [`PauseReason::SESSION_LOCKED`]
/// - `is_remote` -> [`PauseReason::REMOTE_SESSION`]
/// - `is_display_off` -> [`PauseReason::DISPLAY_OFF`]
/// - `is_suspended` -> [`PauseReason::SUSPENDED`]
#[inline]
pub fn evaluate_session_pause(
    is_locked: bool,
    is_remote: bool,
    is_display_off: bool,
    is_suspended: bool,
) -> PauseReason {
    let mut reasons = PauseReason::empty();
    if is_locked {
        reasons.insert(PauseReason::SESSION_LOCKED);
    }
    if is_remote {
        reasons.insert(PauseReason::REMOTE_SESSION);
    }
    if is_display_off {
        reasons.insert(PauseReason::DISPLAY_OFF);
    }
    if is_suspended {
        reasons.insert(PauseReason::SUSPENDED);
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_clear_produces_empty_pause_reason() {
        assert_eq!(
            evaluate_session_pause(false, false, false, false),
            PauseReason::empty()
        );
        let state = SessionState::default();
        assert_eq!(state.evaluate_pause(), PauseReason::empty());
    }

    #[test]
    fn test_individual_locked_state() {
        let reasons = evaluate_session_pause(true, false, false, false);
        assert_eq!(reasons, PauseReason::SESSION_LOCKED);
        assert_eq!(reasons.dominant_reason_label(), Some("Session Locked"));
    }

    #[test]
    fn test_individual_remote_session_state() {
        let reasons = evaluate_session_pause(false, true, false, false);
        assert_eq!(reasons, PauseReason::REMOTE_SESSION);
        assert_eq!(
            reasons.dominant_reason_label(),
            Some("Remote Desktop Active")
        );
    }

    #[test]
    fn test_individual_display_off_state() {
        let reasons = evaluate_session_pause(false, false, true, false);
        assert_eq!(reasons, PauseReason::DISPLAY_OFF);
        assert_eq!(reasons.dominant_reason_label(), Some("Display Turned Off"));
    }

    #[test]
    fn test_individual_suspended_state() {
        let reasons = evaluate_session_pause(false, false, false, true);
        assert_eq!(reasons, PauseReason::SUSPENDED);
        assert_eq!(reasons.dominant_reason_label(), Some("System Suspended"));
    }

    #[test]
    fn test_combination_locked_and_remote() {
        let reasons = evaluate_session_pause(true, true, false, false);
        assert_eq!(
            reasons,
            PauseReason::SESSION_LOCKED | PauseReason::REMOTE_SESSION
        );
        // Session locked takes priority over remote session in user reporting
        assert_eq!(reasons.dominant_reason_label(), Some("Session Locked"));
    }

    #[test]
    fn test_combination_display_off_and_suspended() {
        let reasons = evaluate_session_pause(false, false, true, true);
        assert_eq!(reasons, PauseReason::DISPLAY_OFF | PauseReason::SUSPENDED);
        // Suspended takes priority over display off
        assert_eq!(reasons.dominant_reason_label(), Some("System Suspended"));
    }

    #[test]
    fn test_combination_all_active() {
        let reasons = evaluate_session_pause(true, true, true, true);
        let expected = PauseReason::SESSION_LOCKED
            | PauseReason::REMOTE_SESSION
            | PauseReason::DISPLAY_OFF
            | PauseReason::SUSPENDED;
        assert_eq!(reasons, expected);
        assert_eq!(reasons.dominant_reason_label(), Some("System Suspended"));
    }

    #[test]
    fn test_session_state_struct_builder_and_evaluation() {
        let mut state = SessionState::new(false, false, false, false);
        assert_eq!(state.evaluate_pause(), PauseReason::empty());

        // System locks
        state.is_locked = true;
        assert_eq!(state.evaluate_pause(), PauseReason::SESSION_LOCKED);

        // Display turns off while locked
        state.is_display_off = true;
        assert_eq!(
            state.evaluate_pause(),
            PauseReason::SESSION_LOCKED | PauseReason::DISPLAY_OFF
        );

        // System resumes/display wakes up
        state.is_display_off = false;
        assert_eq!(state.evaluate_pause(), PauseReason::SESSION_LOCKED);

        // User unlocks
        state.is_locked = false;
        assert_eq!(state.evaluate_pause(), PauseReason::empty());
    }

    #[test]
    fn test_remote_session_connect_disconnect_lifecycle() {
        let mut state = SessionState::new(false, false, false, false);
        assert_eq!(state.evaluate_pause(), PauseReason::empty());

        // RDP connects
        state.is_remote = true;
        assert_eq!(state.evaluate_pause(), PauseReason::REMOTE_SESSION);

        // RDP disconnects
        state.is_remote = false;
        assert_eq!(state.evaluate_pause(), PauseReason::empty());
    }

    #[test]
    fn test_suspend_and_resume_lifecycle() {
        let mut state = SessionState::new(false, false, false, false);
        assert_eq!(state.evaluate_pause(), PauseReason::empty());

        // System suspends
        state.is_suspended = true;
        assert_eq!(state.evaluate_pause(), PauseReason::SUSPENDED);

        // System resumes
        state.is_suspended = false;
        assert_eq!(state.evaluate_pause(), PauseReason::empty());
    }
}
