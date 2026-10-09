pub mod claude;
pub mod codex;
pub mod codex_app;

use crate::quota::Snapshot;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceId {
    Codex,
    #[serde(rename = "claude")]
    ClaudeCode,
}

impl ServiceId {
    pub const ALL: [Self; 2] = [Self::Codex, Self::ClaudeCode];

    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
        }
    }

    /// Identifies the service in the status line command and in stored file names.
    pub fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|service| service.key() == key)
    }

    /// The service's own tool hands its usage to the widget while the tool is in use.
    pub fn received(self) -> bool {
        self != Self::Codex
    }
}

/// Keep only the usage numbers from a status line payload.
pub fn extract(service: ServiceId, payload: &Value) -> Option<Value> {
    match service {
        ServiceId::Codex => None,
        ServiceId::ClaudeCode => claude::extract(payload),
    }
}

/// Display data for usage that a service's tool reported earlier.
pub fn received_snapshot(
    service: ServiceId,
    data: &Value,
    received_at: DateTime<Local>,
    now: i64,
) -> Option<Snapshot> {
    match service {
        ServiceId::Codex => None,
        ServiceId::ClaudeCode => Some(claude::snapshot(data, received_at, now)),
    }
}
