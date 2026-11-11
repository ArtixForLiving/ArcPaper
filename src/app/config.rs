//! Persistent application configuration and safe sanitization/validation.

use crate::display::MonitorId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

fn default_legacy_schema_version() -> u32 {
    1
}

fn default_speed() -> f32 {
    1.0
}

fn default_brightness() -> u32 {
    100
}

fn default_fit_mode() -> String {
    "cover".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DisplayMode {
    #[default]
    #[serde(alias = "per_monitor", alias = "per-monitor", alias = "permonitor")]
    PerMonitor,
    #[serde(alias = "clone")]
    Clone,
    #[serde(alias = "span")]
    Span,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MonitorConfigOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_video: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_fps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_manually_paused: Option<bool>,
}

impl MonitorConfigOverride {
    pub fn is_empty(&self) -> bool {
        self.active_video.is_none()
            && self.fit_mode.is_none()
            && self.brightness.is_none()
            && self.speed.is_none()
            && self.target_fps.is_none()
            && self.is_manually_paused.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMonitorConfig {
    pub active_video: Option<String>,
    pub speed: f32,
    pub brightness: u32,
    pub fit_mode: String,
    pub is_manually_paused: bool,
    pub target_fps: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    #[serde(default = "default_legacy_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub active_video: Option<String>,
    #[serde(default = "default_speed")]
    pub speed: f32,
    #[serde(default = "default_brightness")]
    pub brightness: u32,
    #[serde(default = "default_fit_mode")]
    pub fit_mode: String,
    #[serde(default)]
    pub is_manually_paused: bool,
    #[serde(default)]
    pub target_fps: Option<u32>,
    #[serde(default)]
    pub display_mode: DisplayMode,
    #[serde(default)]
    pub monitors: BTreeMap<String, MonitorConfigOverride>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: 2,
            active_video: None,
            speed: default_speed(),
            brightness: default_brightness(),
            fit_mode: default_fit_mode(),
            is_manually_paused: false,
            target_fps: None,
            display_mode: DisplayMode::default(),
            monitors: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    pub fn resolve_for_monitor(&self, monitor_id: &MonitorId) -> ResolvedMonitorConfig {
        let per_mon = self.monitors.get(monitor_id.as_str());
        let effective_paused = self.is_manually_paused
            || per_mon
                .and_then(|ov| ov.is_manually_paused)
                .unwrap_or(false);

        match self.display_mode {
            DisplayMode::Clone | DisplayMode::Span => ResolvedMonitorConfig {
                active_video: self.active_video.clone(),
                speed: self.speed,
                brightness: self.brightness,
                fit_mode: self.fit_mode.clone(),
                is_manually_paused: effective_paused,
                target_fps: self.target_fps,
            },
            DisplayMode::PerMonitor => match per_mon {
                Some(ov) => ResolvedMonitorConfig {
                    active_video: ov
                        .active_video
                        .clone()
                        .or_else(|| self.active_video.clone()),
                    speed: ov.speed.unwrap_or(self.speed),
                    brightness: ov.brightness.unwrap_or(self.brightness),
                    fit_mode: ov.fit_mode.clone().unwrap_or_else(|| self.fit_mode.clone()),
                    is_manually_paused: effective_paused,
                    target_fps: ov.target_fps.or(self.target_fps),
                },
                None => ResolvedMonitorConfig {
                    active_video: self.active_video.clone(),
                    speed: self.speed,
                    brightness: self.brightness,
                    fit_mode: self.fit_mode.clone(),
                    is_manually_paused: effective_paused,
                    target_fps: self.target_fps,
                },
            },
        }
    }
}

/// Normalizes any out-of-range or malformed configuration values to safe defaults,
/// returning the sanitized configuration and a list of diagnostic warnings.
pub fn sanitize_config(mut cfg: AppConfig) -> (AppConfig, Vec<String>) {
    let mut warnings = Vec::new();

    if cfg.schema_version < 2 {
        warnings.push("Migrated configuration schema to v2".to_string());
        cfg.schema_version = 2;
    } else if cfg.schema_version > 2 {
        warnings.push(format!(
            "Unsupported configuration schema_version ({}) clamped to 2",
            cfg.schema_version
        ));
        cfg.schema_version = 2;
    }

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

    let raw_monitors = std::mem::take(&mut cfg.monitors);
    for (key, mut override_cfg) in raw_monitors {
        let trimmed_key = key.trim().to_string();
        if trimmed_key.is_empty() {
            warnings.push("Dropped monitor override entry with empty monitor ID".to_string());
            continue;
        }

        if let Some(ref s) = override_cfg.active_video
            && s.trim().is_empty()
        {
            warnings.push(format!(
                "Monitor '{trimmed_key}': normalized empty active_video to None"
            ));
            override_cfg.active_video = None;
        }

        if let Some(s) = override_cfg.speed
            && (!s.is_finite() || s < 0.1 || s > 4.0)
        {
            warnings.push(format!(
                "Monitor '{trimmed_key}': out-of-range speed ({s}) normalized to None"
            ));
            override_cfg.speed = None;
        }

        if let Some(b) = override_cfg.brightness
            && b > 200
        {
            warnings.push(format!(
                "Monitor '{trimmed_key}': out-of-range brightness ({b}) normalized to None"
            ));
            override_cfg.brightness = None;
        }

        if let Some(ref f) = override_cfg.fit_mode {
            let normalized = f.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "cover" | "fill" | "contain" => {
                    override_cfg.fit_mode = Some(normalized);
                }
                _ => {
                    warnings.push(format!(
                        "Monitor '{trimmed_key}': unsupported fit_mode ('{f}') normalized to None"
                    ));
                    override_cfg.fit_mode = None;
                }
            }
        }

        if let Some(fps) = override_cfg.target_fps
            && (fps == 0 || fps > 240)
        {
            warnings.push(format!(
                "Monitor '{trimmed_key}': out-of-range target_fps ({fps}) normalized to None"
            ));
            override_cfg.target_fps = None;
        }

        if !override_cfg.is_empty() {
            cfg.monitors.insert(trimmed_key, override_cfg);
        }
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
            ..AppConfig::default()
        };

        let (clean, warnings) = sanitize_config(dirty);
        assert_eq!(clean.schema_version, 2);
        assert_eq!(clean.active_video, None);
        assert_eq!(clean.speed, 1.0);
        assert_eq!(clean.brightness, 100);
        assert_eq!(clean.fit_mode, "cover");
        assert!(clean.is_manually_paused);
        assert_eq!(clean.target_fps, None);
        assert_eq!(clean.display_mode, DisplayMode::PerMonitor);
        assert!(clean.monitors.is_empty());
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
        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.active_video.as_deref(), Some("test.mp4"));
        assert_eq!(loaded.speed, 1.5);
        assert_eq!(loaded.brightness, 75);
        assert_eq!(loaded.fit_mode, "contain");
        assert_eq!(loaded.target_fps, None);
        assert_eq!(loaded.display_mode, DisplayMode::PerMonitor);
        assert!(loaded.monitors.is_empty());

        let updated = AppConfig {
            target_fps: Some(30),
            ..loaded
        };
        save_config(&path, &updated);
        let reloaded = load_config(&path);
        assert_eq!(reloaded, updated);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_v1_6_config_migrates_to_v2_preserving_values() {
        let path = temp_config_path("v1_6_migration");
        let v1_6_json = r#"{
            "active_video": "sample.mp4",
            "speed": 2.0,
            "brightness": 75,
            "fit_mode": "contain",
            "is_manually_paused": false,
            "target_fps": 30
        }"#;
        std::fs::write(&path, v1_6_json).unwrap();

        let loaded = load_config(&path);
        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.active_video.as_deref(), Some("sample.mp4"));
        assert_eq!(loaded.speed, 2.0);
        assert_eq!(loaded.brightness, 75);
        assert_eq!(loaded.fit_mode, "contain");
        assert!(!loaded.is_manually_paused);
        assert_eq!(loaded.target_fps, Some(30));
        assert_eq!(loaded.display_mode, DisplayMode::PerMonitor);
        assert!(loaded.monitors.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_sanitize_per_monitor_overrides_cleans_invalid_and_empty() {
        let mut monitors = std::collections::BTreeMap::new();

        monitors.insert(
            "   ".to_string(),
            MonitorConfigOverride {
                speed: Some(1.5),
                ..Default::default()
            },
        );

        monitors.insert(
            "  MON_ALL_INVALID  ".to_string(),
            MonitorConfigOverride {
                active_video: Some("   ".to_string()),
                speed: Some(f32::NAN),
                brightness: Some(999),
                fit_mode: Some("bad_mode".to_string()),
                target_fps: Some(0),
                is_manually_paused: None,
            },
        );

        monitors.insert(
            "  MON_VALID  ".to_string(),
            MonitorConfigOverride {
                active_video: Some("video.mp4".to_string()),
                speed: Some(1.25),
                brightness: Some(150),
                fit_mode: Some("FILL".to_string()),
                target_fps: Some(60),
                is_manually_paused: Some(true),
            },
        );

        monitors.insert(
            "MON_PARTIAL".to_string(),
            MonitorConfigOverride {
                active_video: None,
                speed: Some(99.0),
                brightness: Some(50),
                fit_mode: None,
                target_fps: Some(999),
                is_manually_paused: None,
            },
        );

        monitors.insert(
            "MON_TOO_SLOW".to_string(),
            MonitorConfigOverride {
                speed: Some(0.05),
                ..Default::default()
            },
        );

        let dirty = AppConfig {
            schema_version: 1,
            monitors,
            ..AppConfig::default()
        };

        let (clean, warnings) = sanitize_config(dirty);

        assert_eq!(clean.schema_version, 2);
        assert!(!clean.monitors.contains_key("   "));
        assert!(!clean.monitors.contains_key("  MON_ALL_INVALID  "));
        assert!(!clean.monitors.contains_key("MON_ALL_INVALID"));
        assert!(!clean.monitors.contains_key("MON_TOO_SLOW"));

        let valid_entry = clean
            .monitors
            .get("MON_VALID")
            .expect("trimmed key MON_VALID must exist");
        assert_eq!(valid_entry.active_video.as_deref(), Some("video.mp4"));
        assert_eq!(valid_entry.speed, Some(1.25));
        assert_eq!(valid_entry.brightness, Some(150));
        assert_eq!(valid_entry.fit_mode.as_deref(), Some("fill"));
        assert_eq!(valid_entry.target_fps, Some(60));
        assert_eq!(valid_entry.is_manually_paused, Some(true));

        let partial_entry = clean
            .monitors
            .get("MON_PARTIAL")
            .expect("MON_PARTIAL must exist");
        assert_eq!(partial_entry.speed, None);
        assert_eq!(partial_entry.brightness, Some(50));
        assert_eq!(partial_entry.target_fps, None);

        assert!(!warnings.is_empty());
    }

    #[test]
    fn test_resolve_for_monitor_handles_modes_and_unknown_monitor() {
        let mon1 = crate::display::MonitorId::from("MON_1");
        let mon2 = crate::display::MonitorId::from("MON_2");
        let unknown = crate::display::MonitorId::from("MON_UNKNOWN");

        let mut monitors = std::collections::BTreeMap::new();
        monitors.insert(
            "MON_1".to_string(),
            MonitorConfigOverride {
                active_video: Some("mon1.mp4".to_string()),
                speed: Some(2.0),
                brightness: Some(150),
                fit_mode: Some("contain".to_string()),
                target_fps: Some(120),
                is_manually_paused: Some(true),
            },
        );
        monitors.insert(
            "MON_2".to_string(),
            MonitorConfigOverride {
                is_manually_paused: Some(false),
                ..Default::default()
            },
        );

        let mut cfg = AppConfig {
            active_video: Some("global.mp4".to_string()),
            speed: 1.0,
            brightness: 100,
            fit_mode: "cover".to_string(),
            is_manually_paused: false,
            target_fps: Some(60),
            display_mode: DisplayMode::PerMonitor,
            monitors,
            ..AppConfig::default()
        };

        let res1 = cfg.resolve_for_monitor(&mon1);
        assert_eq!(
            res1,
            ResolvedMonitorConfig {
                active_video: Some("mon1.mp4".to_string()),
                speed: 2.0,
                brightness: 150,
                fit_mode: "contain".to_string(),
                is_manually_paused: true,
                target_fps: Some(120),
            }
        );

        let res_unknown = cfg.resolve_for_monitor(&unknown);
        assert_eq!(
            res_unknown,
            ResolvedMonitorConfig {
                active_video: Some("global.mp4".to_string()),
                speed: 1.0,
                brightness: 100,
                fit_mode: "cover".to_string(),
                is_manually_paused: false,
                target_fps: Some(60),
            }
        );

        cfg.is_manually_paused = true;
        let res2 = cfg.resolve_for_monitor(&mon2);
        assert!(res2.is_manually_paused);

        cfg.is_manually_paused = false;

        cfg.display_mode = DisplayMode::Clone;
        let res_clone = cfg.resolve_for_monitor(&mon1);
        assert_eq!(res_clone.active_video.as_deref(), Some("global.mp4"));
        assert_eq!(res_clone.speed, 1.0);
        assert_eq!(res_clone.brightness, 100);
        assert_eq!(res_clone.fit_mode, "cover");
        assert_eq!(res_clone.target_fps, Some(60));
        assert!(res_clone.is_manually_paused);

        cfg.display_mode = DisplayMode::Span;
        let res_span = cfg.resolve_for_monitor(&mon1);
        assert_eq!(res_span.active_video.as_deref(), Some("global.mp4"));
        assert_eq!(res_span.speed, 1.0);
        assert_eq!(res_span.brightness, 100);
        assert_eq!(res_span.fit_mode, "cover");
        assert_eq!(res_span.target_fps, Some(60));
        assert!(res_span.is_manually_paused);
    }
}
