#![no_main]

use arc_paper::app::config::{AppConfig, sanitize_config};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(cfg) = serde_json::from_slice::<AppConfig>(data) {
        let (sanitized, _warnings) = sanitize_config(cfg);
        // Enforce post-sanitization schema v3 invariants
        assert_eq!(sanitized.schema_version, 3);
        assert!(
            sanitized.power.low_battery_threshold >= 5
                && sanitized.power.low_battery_threshold <= 50
        );
        assert!(sanitized.speed.is_finite() && sanitized.speed >= 0.1 && sanitized.speed <= 4.0);
        assert!(sanitized.brightness <= 200);

        // Ensure round-trip serialization never panics
        let _ = serde_json::to_vec(&sanitized);
    }
});
