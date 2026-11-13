//! Windows accessibility Reduce Motion / Animation detection (FR-305).
//!
//! Respects the Windows "Show animations in Windows" setting via
//! `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, ...)`.

use crate::app::pause::PauseReason;

#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

/// Queries whether client area animations are enabled in Windows system settings.
///
/// Returns `true` if animations are enabled, or defaults to `true` if the query fails or on non-Windows.
pub fn is_client_area_animation_enabled() -> bool {
    #[cfg(windows)]
    {
        use windows::core::BOOL;
        let mut anim_val = BOOL(0);
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some(&mut anim_val as *mut _ as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        if ok.is_ok() {
            return anim_val.as_bool();
        }
    }
    true
}

/// Pure helper determining whether Reduce Motion pause should be active.
///
/// Returns `true` if the user opted in (`follow_reduce_motion == true`) AND
/// Windows client area animations are disabled (`animations_enabled == false`).
#[inline]
pub fn is_reduce_motion_active(follow_reduce_motion: bool, animations_enabled: bool) -> bool {
    follow_reduce_motion && !animations_enabled
}

/// Evaluates whether Reduce Motion requires pausing wallpaper playback.
///
/// Returns `PauseReason::REDUCE_MOTION` if active, otherwise `PauseReason::empty()`.
#[inline]
pub fn evaluate_reduce_motion_pause(
    follow_reduce_motion: bool,
    animations_enabled: bool,
) -> PauseReason {
    if is_reduce_motion_active(follow_reduce_motion, animations_enabled) {
        PauseReason::REDUCE_MOTION
    } else {
        PauseReason::empty()
    }
}

/// Queries Windows setting and returns `true` if `follow_reduce_motion` is enabled
/// and client area animations are disabled in Windows.
pub fn is_reduce_motion_enabled(follow_reduce_motion: bool) -> bool {
    if !follow_reduce_motion {
        false
    } else {
        !is_client_area_animation_enabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reduce_motion_disabled_when_follow_is_false() {
        assert!(!is_reduce_motion_active(false, true));
        assert!(!is_reduce_motion_active(false, false));
        assert_eq!(
            evaluate_reduce_motion_pause(false, true),
            PauseReason::empty()
        );
        assert_eq!(
            evaluate_reduce_motion_pause(false, false),
            PauseReason::empty()
        );
    }

    #[test]
    fn test_reduce_motion_enabled_when_follow_is_true_and_animations_disabled() {
        // Follow enabled + animations disabled -> Reduce motion active
        assert!(is_reduce_motion_active(true, false));
        assert_eq!(
            evaluate_reduce_motion_pause(true, false),
            PauseReason::REDUCE_MOTION
        );

        // Follow enabled + animations enabled -> Reduce motion NOT active
        assert!(!is_reduce_motion_active(true, true));
        assert_eq!(
            evaluate_reduce_motion_pause(true, true),
            PauseReason::empty()
        );
    }

    #[test]
    fn test_is_reduce_motion_enabled_with_follow_false() {
        // When follow_reduce_motion is false, it always returns false without querying Windows
        assert!(!is_reduce_motion_enabled(false));
    }

    #[test]
    fn test_is_client_area_animation_enabled_smoke() {
        // Verifies calling the API does not panic or crash
        let _enabled = is_client_area_animation_enabled();
    }
}
