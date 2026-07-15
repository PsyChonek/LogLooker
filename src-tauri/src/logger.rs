use crate::models::LogEntry;
use std::sync::Mutex;

pub struct LogState(pub Mutex<Vec<LogEntry>>);

impl LogState {
    pub fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    pub fn add(&self, level: &str, source: &str, message: &str) {
        if let Ok(mut logs) = self.0.lock() {
            logs.push(LogEntry {
                timestamp: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                level: level.to_string(),
                source: source.to_string(),
                message: message.to_string(),
            });
            // Keep last 500 entries
            if logs.len() > 500 {
                let drain_count = logs.len() - 500;
                logs.drain(0..drain_count);
            }
        }
    }

    pub fn info(&self, source: &str, message: &str) {
        self.add("info", source, message);
    }

    #[allow(dead_code)]
    pub fn error(&self, source: &str, message: &str) {
        self.add("error", source, message);
    }

    #[allow(dead_code)]
    pub fn warn(&self, source: &str, message: &str) {
        self.add("warn", source, message);
    }
}
