//! Persistent application configuration and safe sanitization/validation.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    pub active_video: Option<String>,
    pub speed: f32,
    pub brightness: u32,
    pub fit_mode: String,
    pub is_manually_paused: bool,
    #[serde(default)]
    pub target_fps: Option<u32>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_video: None,
            speed: 1.0,
            brightness: 100,
            fit_mode: "cover".to_string(),
            is_manually_paused: false,
            target_fps: None,
        }
    }
}

/// Normalizes any out-of-range or malformed configuration values to safe defaults,
/// returning the sanitized configuration and a list of diagnostic warnings.
pub fn sanitize_config(mut cfg: AppConfig) -> (AppConfig, Vec<String>) {
    let mut warnings = Vec::new();

    if let Some(ref name) = cfg.active_video
        && name.trim().is_empty()
    {
        cfg.active_video = None;
        warnings.push("Normalized empty active_video to None".to_string());
    }

    if !cfg.speed.is_finite() || cfg.speed < 0.1 || cfg.speed > 4.0 {
        warnings.push(format!(
            "Out-of-range speed ({}) normalized to 1.0",
            cfg.speed
        ));
        cfg.speed = 1.0;
    }

    if cfg.brightness > 200 {
        warnings.push(format!(
            "Out-of-range brightness ({}) normalized to 100",
            cfg.brightness
        ));
        cfg.brightness = 100;
    }

    let normalized_fit = cfg.fit_mode.trim().to_ascii_lowercase();
    match normalized_fit.as_str() {
        "cover" | "fill" | "contain" => {
            if cfg.fit_mode != normalized_fit {
                cfg.fit_mode = normalized_fit;
            }
        }
        _ => {
            warnings.push(format!(
                "Unsupported fit_mode ('{}') normalized to 'cover'",
                cfg.fit_mode
            ));
            cfg.fit_mode = "cover".to_string();
        }
    }

    if let Some(fps) = cfg.target_fps
        && (fps == 0 || fps > 240)
    {
        warnings.push(format!(
            "Out-of-range target_fps ({fps}) normalized to None (native)"
        ));
        cfg.target_fps = None;
    }

    (cfg, warnings)
}

/// Loads `config.json` from `path`, sanitizing values or falling back to defaults if missing/corrupt.
pub fn load_config(path: &Path) -> AppConfig {
    use crate::app::log::{LogLevel, log_with_level};

    if !path.is_file() {
        log_with_level(
            LogLevel::Info,
            "[ArcPaper] No existing config found, using default configuration.",
        );
        return AppConfig::default();
    }

    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<AppConfig>(&content) {
            Ok(parsed) => {
                let (clean, warnings) = sanitize_config(parsed);
                for w in &warnings {
                    log_with_level(LogLevel::Warn, &format!("[ArcPaper] Config: {w}"));
                }
                log_with_level(
                    LogLevel::Info,
                    &format!("[ArcPaper] Loaded configuration from {:?}", path),
                );
                clean
            }
            Err(e) => {
                log_with_level(
                    LogLevel::Warn,
                    &format!(
                        "[ArcPaper] Failed to parse config {:?} ({}); falling back to defaults.",
                        path, e
                    ),
                );
                AppConfig::default()
            }
        },
        Err(e) => {
            log_with_level(
                LogLevel::Warn,
                &format!(
                    "[ArcPaper] Failed to read config {:?} ({}); falling back to defaults.",
                    path, e
                ),
            );
            AppConfig::default()
        }
    }
}

/// Serializes `config` to `path` as pretty-printed JSON.
pub fn save_config(path: &Path, config: &AppConfig) {
    use crate::app::log::{LogLevel, log_with_level};

    if let Ok(json) = serde_json::to_string_pretty(config) {
        if let Err(e) = std::fs::write(path, json) {
            log_with_level(
                LogLevel::Error,
                &format!("[ArcPaper] Error saving config to {:?}: {:?}", path, e),
            );
        } else {
            log_with_level(
                LogLevel::Info,
                &format!("[ArcPaper] Successfully saved config to {:?}", path),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "arc_paper_cfg_{name}_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn test_sanitize_config_normalizes_extreme_values() {
        let dirty = AppConfig {
            active_video: Some("   ".to_string()),
            speed: -5.0,
            brightness: 9999,
            fit_mode: "WIDESCREEN".to_string(),
            is_manually_paused: true,
            target_fps: Some(0),
        };

        let (clean, warnings) = sanitize_config(dirty);
        assert_eq!(clean.active_video, None);
        assert_eq!(clean.speed, 1.0);
        assert_eq!(clean.brightness, 100);
        assert_eq!(clean.fit_mode, "cover");
        assert!(clean.is_manually_paused);
        assert_eq!(clean.target_fps, None);
        assert!(warnings.len() >= 4);

        let nan_cfg = AppConfig {
            speed: f32::NAN,
            fit_mode: "CoNtAiN".to_string(),
            target_fps: Some(999),
            ..Default::default()
        };
        let (clean2, warnings2) = sanitize_config(nan_cfg);
        assert_eq!(clean2.speed, 1.0);
        assert_eq!(clean2.fit_mode, "contain");
        assert_eq!(clean2.target_fps, None);
        assert!(!warnings2.is_empty());
    }

    #[test]
    fn test_load_config_corrupt_json_falls_back_to_default() {
        let path = temp_config_path("corrupt");
        std::fs::write(&path, "{ invalid json ::: [").unwrap();

        let cfg = load_config(&path);
        assert_eq!(cfg, AppConfig::default());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_config_legacy_without_target_fps_and_roundtrip() {
        let path = temp_config_path("legacy");
        let legacy_json = r#"{
            "active_video": "test.mp4",
            "speed": 1.5,
            "brightness": 75,
            "fit_mode": "contain",
            "is_manually_paused": false
        }"#;
        std::fs::write(&path, legacy_json).unwrap();

        let loaded = load_config(&path);
        assert_eq!(loaded.active_video.as_deref(), Some("test.mp4"));
        assert_eq!(loaded.speed, 1.5);
        assert_eq!(loaded.brightness, 75);
        assert_eq!(loaded.fit_mode, "contain");
        assert_eq!(loaded.target_fps, None);

        let updated = AppConfig {
            target_fps: Some(30),
            ..loaded
        };
        save_config(&path, &updated);
        let reloaded = load_config(&path);
        assert_eq!(reloaded, updated);

        let _ = std::fs::remove_file(&path);
    }
}
