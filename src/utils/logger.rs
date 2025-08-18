use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::OnceLock;
use serde::{Serialize, Deserialize};
use std::io::Write;

static LOG_FORMAT: OnceLock<LogFormat> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogFormat {
    Console,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Serialize)]
struct LogEntry {
    timestamp: u64,
    level: LogLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    container_id: Option<String>,
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
}

pub struct Logger;

impl Logger {
    /// Initialize logger with format from environment
    pub fn init() {
        let format = std::env::var("QUILT_LOG_FORMAT")
            .ok()
            .and_then(|s| match s.to_lowercase().as_str() {
                "json" => Some(LogFormat::Json),
                "console" => Some(LogFormat::Console),
                _ => None,
            })
            .unwrap_or(LogFormat::Console);
        
        LOG_FORMAT.set(format).ok();
    }

    fn get_format() -> LogFormat {
        *LOG_FORMAT.get().unwrap_or(&LogFormat::Console)
    }

    fn timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    pub fn log(
        level: LogLevel,
        container_id: Option<&str>,
        event: &str,
        details: Option<serde_json::Value>,
        duration_ms: Option<u64>,
    ) {
        let entry = LogEntry {
            timestamp: Self::timestamp(),
            level,
            container_id: container_id.map(String::from),
            event: event.to_string(),
            details,
            duration_ms,
        };

        match Self::get_format() {
            LogFormat::Json => {
                if let Ok(json) = serde_json::to_string(&entry) {
                    let _ = writeln!(std::io::stdout(), "{}", json);
                }
            }
            LogFormat::Console => {
                let level_str = match level {
                    LogLevel::Debug => "DEBUG",
                    LogLevel::Info => "INFO",
                    LogLevel::Warn => "WARN",
                    LogLevel::Error => "ERROR",
                };

                let timestamp = humantime::format_rfc3339_millis(
                    UNIX_EPOCH + std::time::Duration::from_millis(entry.timestamp)
                );

                let mut output = format!("[{}] {} {}", timestamp, level_str, event);
                
                if let Some(id) = container_id {
                    output.push_str(&format!(" [{}]", id));
                }
                
                if let Some(ms) = duration_ms {
                    output.push_str(&format!(" ({}ms)", ms));
                }
                
                if let Some(ref details) = entry.details {
                    output.push_str(&format!(" {}", details));
                }

                let _ = writeln!(std::io::stdout(), "{}", output);
            }
        }
    }

    // Convenience methods
    pub fn debug(event: &str) {
        Self::log(LogLevel::Debug, None, event, None, None);
    }

    pub fn info(event: &str) {
        Self::log(LogLevel::Info, None, event, None, None);
    }

    pub fn warn(event: &str) {
        Self::log(LogLevel::Warn, None, event, None, None);
    }

    pub fn error(event: &str) {
        Self::log(LogLevel::Error, None, event, None, None);
    }

    pub fn container_event(
        level: LogLevel,
        container_id: &str,
        event: &str,
        details: Option<serde_json::Value>,
    ) {
        Self::log(level, Some(container_id), event, details, None);
    }

    pub fn timed_operation<F, R>(
        level: LogLevel,
        container_id: Option<&str>,
        event: &str,
        operation: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let start = SystemTime::now();
        let result = operation();
        let duration_ms = start.elapsed().unwrap_or_default().as_millis() as u64;
        
        Self::log(level, container_id, event, None, Some(duration_ms));
        result
    }
}

/// Timing helper for measuring operation duration
pub struct Timer {
    start: SystemTime,
    event: String,
    container_id: Option<String>,
}

impl Timer {
    pub fn new(event: &str) -> Self {
        Self {
            start: SystemTime::now(),
            event: event.to_string(),
            container_id: None,
        }
    }

    pub fn with_container(event: &str, container_id: &str) -> Self {
        Self {
            start: SystemTime::now(),
            event: event.to_string(),
            container_id: Some(container_id.to_string()),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().unwrap_or_default().as_millis() as u64
    }

    pub fn log_completion(self, level: LogLevel) {
        Logger::log(
            level,
            self.container_id.as_deref(),
            &self.event,
            None,
            Some(self.elapsed_ms()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_format_parsing() {
        std::env::set_var("QUILT_LOG_FORMAT", "json");
        Logger::init();
        assert_eq!(Logger::get_format(), LogFormat::Json);
    }

    #[test]
    fn test_timer() {
        let timer = Timer::new("test_operation");
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(timer.elapsed_ms() >= 10);
    }
}