//! A small summary of the whole fleet that the running dashboard writes for the status line:
//! who needs the user, how many agents work, and per seat where its account's week stands. The
//! status line runs after every response and must not open the index; it reads this file
//! instead, and falls back to its own cheap view when the file is stale (dashboard not running).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Fleet {
    pub v: u32,
    pub written_at: DateTime<Utc>,
    pub writer_pid: u32,
    pub needs_you: Vec<NeedsYou>,
    pub counts: Counts,
    /// Per session id: its agents (the main thread not counted) and its spend rate.
    pub sessions: BTreeMap<String, SessionCounts>,
    /// Per seat, keyed like the status-line reading files (`statusline::file_for`).
    pub seats: BTreeMap<String, SeatLimit>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NeedsYou {
    pub session_id: String,
    pub seat: String,
    /// Session title, else its project.
    pub name: String,
    pub what: String,
    pub since: Option<DateTime<Utc>>,
    pub tmux: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Counts {
    pub sessions: u32,
    pub agents: u32,
    pub working: u32,
    pub thinking: u32,
    pub waiting: u32,
    pub stalled: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionCounts {
    pub agents: u32,
    pub working: u32,
    pub thinking: u32,
    pub waiting: u32,
    pub stalled: u32,
    pub burn_usd_h: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SeatLimit {
    pub label: String,
    pub account: String,
    pub week_used: Option<f64>,
    pub runs_out_at: Option<DateTime<Utc>>,
    pub resets_at: Option<DateTime<Utc>>,
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("statusline").join("fleet.json")
}

/// Atomic write, readable by the user only.
pub fn write(path: &Path, f: &Fleet) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(f).unwrap_or_default())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
}

/// The summary when it is younger than `fresh_seconds` and its writer still runs.
pub fn read_fresh(path: &Path, fresh_seconds: u32, now: DateTime<Utc>) -> Option<Fleet> {
    let f: Fleet = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if f.v != VERSION || (now - f.written_at).num_seconds() > fresh_seconds as i64 {
        return None;
    }
    if cfg!(target_os = "linux") && !Path::new("/proc").join(f.writer_pid.to_string()).exists() {
        return None;
    }
    Some(f)
}
