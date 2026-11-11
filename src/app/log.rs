//! Leveled, throttled, and size-rotating file logger.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default maximum size (1 MiB) before `app.log` is rotated.
pub const DEFAULT_MAX_LOG_BYTES: u64 = 1024 * 1024;
/// Default number of rotated backup files retained (`app.log.1` .. `app.log.3`).
pub const DEFAULT_MAX_LOG_BACKUPS: usize = 3;

static GLOBAL_LOGGER: Mutex<Option<RotatingLogger>> = Mutex::new(None);

/// Severity level for diagnostic log entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
        }
    }
}

/// Thread-safe rotating file logger with per-key rate limiting.
pub struct RotatingLogger {
    log_dir: PathBuf,
    max_bytes: u64,
    max_backups: usize,
    last_throttled: HashMap<&'static str, Instant>,
}

impl RotatingLogger {
    /// Creates a new logger targeting `log_dir/app.log`.
    pub fn new(log_dir: PathBuf, max_bytes: u64, max_backups: usize) -> Self {
        Self {
            log_dir,
            max_bytes: max_bytes.max(64),
            max_backups,
            last_throttled: HashMap::new(),
        }
    }

    pub fn log_path(&self) -> PathBuf {
        self.log_dir.join("app.log")
    }

    fn backup_path(&self, index: usize) -> PathBuf {
        self.log_dir.join(format!("app.log.{index}"))
    }

    fn rotate_if_needed(&self, incoming_bytes: u64) {
        let active = self.log_path();
        let current_len = fs::metadata(&active).map(|m| m.len()).unwrap_or(0);
        if current_len == 0 || current_len.saturating_add(incoming_bytes) <= self.max_bytes {
            return;
        }

        if self.max_backups == 0 {
            let _ = fs::remove_file(&active);
            return;
        }

        let oldest = self.backup_path(self.max_backups);
        if oldest.exists() {
            let _ = fs::remove_file(&oldest);
        }

        for idx in (1..self.max_backups).rev() {
            let src = self.backup_path(idx);
            let dst = self.backup_path(idx + 1);
            if src.exists() {
                let _ = fs::rename(&src, &dst);
            }
        }

        let _ = fs::rename(&active, self.backup_path(1));
    }

    /// Formats and appends a single log entry, rotating files when the size limit is exceeded.
    pub fn write(&mut self, level: LogLevel, msg: &str) {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let line = format!("[{ts_ms}][{}] {msg}\n", level.as_str());

        let _ = fs::create_dir_all(&self.log_dir);
        self.rotate_if_needed(line.len() as u64);

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())
        {
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// Writes a log entry only if `cooldown` has elapsed since the previous entry for `key`.
    pub fn write_throttled(
        &mut self,
        key: &'static str,
        cooldown: Duration,
        level: LogLevel,
        msg: &str,
    ) -> bool {
        let now = Instant::now();
        if let Some(&last) = self.last_throttled.get(key)
            && now.duration_since(last) < cooldown
        {
            return false;
        }
        self.last_throttled.insert(key, now);
        self.write(level, msg);
        true
    }
}

fn default_log_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        return parent.to_path_buf();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Configures the global logger directory, size limit, and backup count.
pub fn init_logger(log_dir: PathBuf, max_bytes: u64, max_backups: usize) {
    if let Ok(mut guard) = GLOBAL_LOGGER.lock() {
        *guard = Some(RotatingLogger::new(log_dir, max_bytes, max_backups));
    }
}

/// Logs a message at the given severity level using the global logger.
pub fn log_with_level(level: LogLevel, msg: &str) {
    if let Ok(mut guard) = GLOBAL_LOGGER.lock() {
        let logger = guard.get_or_insert_with(|| {
            RotatingLogger::new(
                default_log_dir(),
                DEFAULT_MAX_LOG_BYTES,
                DEFAULT_MAX_LOG_BACKUPS,
            )
        });
        logger.write(level, msg);
    }
}

/// Logs a rate-limited message keyed by `key` using the global logger.
pub fn log_throttled(key: &'static str, cooldown: Duration, level: LogLevel, msg: &str) {
    if let Ok(mut guard) = GLOBAL_LOGGER.lock() {
        let logger = guard.get_or_insert_with(|| {
            RotatingLogger::new(
                default_log_dir(),
                DEFAULT_MAX_LOG_BYTES,
                DEFAULT_MAX_LOG_BACKUPS,
            )
        });
        logger.write_throttled(key, cooldown, level, msg);
    }
}

/// Logs an informational message using the global logger.
pub fn log_msg(msg: &str) {
    log_with_level(LogLevel::Info, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arc_paper_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    #[test]
    fn test_log_levels_and_rotation() {
        let dir = temp_test_dir("log_rotation");
        let mut logger = RotatingLogger::new(dir.clone(), 120, 2);

        for i in 0..6 {
            let level = match i % 3 {
                0 => LogLevel::Error,
                1 => LogLevel::Warn,
                _ => LogLevel::Info,
            };
            logger.write(
                level,
                &format!("diagnostic message index {i} padding_payload_here"),
            );
        }

        let active = std::fs::read_to_string(dir.join("app.log")).expect("app.log should exist");
        assert!(!active.is_empty());
        assert!(dir.join("app.log.1").exists(), "first backup should exist");
        assert!(dir.join("app.log.2").exists(), "second backup should exist");
        assert!(
            !dir.join("app.log.3").exists(),
            "backups beyond max_backups=2 must not exist"
        );

        let all_logs = format!(
            "{}{}{}",
            active,
            std::fs::read_to_string(dir.join("app.log.1")).unwrap_or_default(),
            std::fs::read_to_string(dir.join("app.log.2")).unwrap_or_default()
        );
        assert!(all_logs.contains("[ERROR]"));
        assert!(all_logs.contains("[WARN]"));
        assert!(all_logs.contains("[INFO]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_throttled_logging_suppresses_repeats() {
        let dir = temp_test_dir("log_throttle");
        let mut logger = RotatingLogger::new(dir.clone(), 1024 * 1024, 1);

        assert!(logger.write_throttled(
            "render_err",
            Duration::from_secs(60),
            LogLevel::Error,
            "first error"
        ));
        assert!(!logger.write_throttled(
            "render_err",
            Duration::from_secs(60),
            LogLevel::Error,
            "duplicate suppressed"
        ));
        assert!(logger.write_throttled(
            "present_err",
            Duration::from_secs(60),
            LogLevel::Warn,
            "different key allowed"
        ));

        let contents = std::fs::read_to_string(dir.join("app.log")).expect("app.log should exist");
        assert!(contents.contains("first error"));
        assert!(!contents.contains("duplicate suppressed"));
        assert!(contents.contains("different key allowed"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
