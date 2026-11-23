//! Claude Code's status-line input: the JSON it passes on stdin each time it runs
//! `statusLine.command` (an anti-corruption layer: the rest of the status line sees these types
//! only). Format of Claude Code 2.1.329: `rate_limits` is present only for subscribers, after
//! the first API response, while a window is active; `resets_at` is in epoch seconds.

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

/// The part of Claude Code's status-line input that swarmy uses.
#[derive(Debug, Default, Deserialize)]
pub struct Input {
    #[serde(default)]
    pub session_id: Option<String>,
    /// The session's transcript, read from its end for the last response's owner and time.
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub model: Option<Model>,
    #[serde(default)]
    pub cost: Option<Cost>,
    #[serde(default)]
    pub context_window: Option<ContextWindow>,
    #[serde(default)]
    pub rate_limits: Option<RateLimits>,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub prompt_cache: Option<PromptCache>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Effort {
    #[serde(default)]
    pub level: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Workspace {
    #[serde(default)]
    pub current_dir: Option<String>,
    #[serde(default)]
    pub git_worktree: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PromptCache {
    #[serde(default)]
    pub warm: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Model {
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Cost {
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ContextWindow {
    #[serde(default)]
    pub used_percentage: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RateLimits {
    #[serde(default)]
    pub five_hour: Option<Window>,
    #[serde(default)]
    pub seven_day: Option<Window>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Window {
    pub used_percentage: f64,
    /// Epoch seconds.
    pub resets_at: f64,
}

impl Window {
    pub(super) fn pair(&self) -> Option<(f64, DateTime<Utc>)> {
        let t = Utc.timestamp_opt(self.resets_at as i64, 0).single()?;
        Some((self.used_percentage, t))
    }
}

pub fn parse_input(text: &str) -> Input {
    serde_json::from_str(text).unwrap_or_default()
}
