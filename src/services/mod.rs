pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod codex_app;

use crate::quota::Snapshot;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceId {
    Codex,
    #[serde(rename = "claude")]
    ClaudeCode,
    #[serde(rename = "antigravity")]
    AntigravityCli,
}

impl ServiceId {
    pub const ALL: [Self; 3] = [Self::Codex, Self::ClaudeCode, Self::AntigravityCli];

    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::AntigravityCli => "Antigravity CLI",
        }
    }

    /// Names the service in the one-line bar.
    pub fn short_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude",
            Self::AntigravityCli => "Antigravity",
        }
    }

    /// Identifies the service in the status line command and in stored file names.
    pub fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
            Self::AntigravityCli => "antigravity",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|service| service.key() == key)
    }

    /// The service's own tool hands its usage to the widget while the tool is in use.
    pub fn received(self) -> bool {
        self != Self::Codex
    }

    /// Where the service's tool keeps its settings, including its status line command.
    pub fn config_dir(self) -> Option<PathBuf> {
        match self {
            Self::Codex => None,
            Self::ClaudeCode => claude::config_dir(),
            Self::AntigravityCli => antigravity::config_dir(),
        }
    }

    /// Whether the service seems to be installed; only checks that its files exist.
    pub fn detected(self) -> bool {
        match self {
            Self::Codex => codex::find_codex(None).is_some(),
            _ => self
                .config_dir()
                .is_some_and(|directory| directory.is_dir()),
        }
    }
}

/// Keep only the usage numbers from a status line payload.
pub fn extract(service: ServiceId, payload: &Value) -> Option<Value> {
    match service {
        ServiceId::Codex => None,
        ServiceId::ClaudeCode => claude::extract(payload),
        ServiceId::AntigravityCli => antigravity::extract(payload),
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
        ServiceId::AntigravityCli => Some(antigravity::snapshot(data, received_at, now)),
    }
}
