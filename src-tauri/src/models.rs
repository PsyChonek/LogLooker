use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    /// "info" | "warn" | "error"
    pub level: String,
    pub source: String,
    pub message: String,
}
