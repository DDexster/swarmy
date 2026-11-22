//! Claude Code's registry of live sessions: `<config dir>/sessions/<pid>.json`.
//!
//! Each interactive Claude Code writes its pid, session id, cwd, name, tmux pane and a status
//! (`busy`, `idle`, `waiting` with `waitingFor`) there. Files of dead processes linger, so a
//! session counts as live only when its pid runs and, on Linux, the start time still matches.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use super::procs::{ProcTable, proc_start_ticks};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawSession {
    pid: u32,
    session_id: String,
    cwd: String,
    started_at: i64,
    proc_start: Option<String>,
    version: Option<String>,
    kind: Option<String>,
    entrypoint: Option<String>,
    tmux: Option<String>,
    name: Option<String>,
    status: Option<String>,
    waiting_for: Option<String>,
    updated_at: Option<i64>,
    status_updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveSession {
    pub seat: String,
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
    pub name: Option<String>,
    pub status: SessionStatus,
    pub tmux: Option<String>,
    pub version: Option<String>,
    pub kind: Option<String>,
    pub started_at: DateTime<Utc>,
    pub status_since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SessionStatus {
    Busy,
    Idle,
    /// Blocked on the user: a permission prompt, a question, a dialog…
    Waiting(String),
    Other(String),
}

impl SessionStatus {
    fn parse(status: Option<&str>, waiting_for: Option<&str>) -> Self {
        match status.unwrap_or("") {
            "busy" => SessionStatus::Busy,
            "idle" | "" => SessionStatus::Idle,
            "waiting" => SessionStatus::Waiting(waiting_for.unwrap_or("input").to_string()),
            s => SessionStatus::Other(s.to_string()),
        }
    }
}

fn ms(t: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(t).single()
}

/// The pids named in a seat's registry files (alive or not).
pub fn session_pids(sessions_dir: &Path) -> Vec<u32> {
    let Ok(rd) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".json").and_then(|n| n.parse().ok())
        })
        .collect()
}

/// Live sessions of one seat. Stale files are ignored, never deleted.
pub fn live_sessions(seat_label: &str, sessions_dir: &Path, procs: &ProcTable) -> Vec<LiveSession> {
    let Ok(rd) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(r) = serde_json::from_str::<RawSession>(&text) else {
            continue;
        };
        if r.pid == 0 || !procs.alive(r.pid) {
            continue;
        }
        if let (Some(want), Some(have)) = (&r.proc_start, proc_start_ticks(r.pid))
            && *want != have
        {
            continue; // the pid was reused by another process
        }
        out.push(LiveSession {
            seat: seat_label.to_string(),
            pid: r.pid,
            session_id: r.session_id,
            cwd: r.cwd,
            name: r.name,
            status: SessionStatus::parse(r.status.as_deref(), r.waiting_for.as_deref()),
            tmux: r.tmux.filter(|t| !t.is_empty()),
            version: r.version,
            kind: r.kind.or(r.entrypoint),
            started_at: ms(r.started_at).unwrap_or_else(Utc::now),
            status_since: r.status_updated_at.or(r.updated_at).and_then(ms),
        });
    }
    out.sort_by_key(|s| s.started_at);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing() {
        assert_eq!(
            SessionStatus::parse(Some("busy"), None),
            SessionStatus::Busy
        );
        assert_eq!(
            SessionStatus::parse(Some("waiting"), Some("input needed")),
            SessionStatus::Waiting("input needed".into())
        );
        assert_eq!(SessionStatus::parse(None, None), SessionStatus::Idle);
    }

    #[test]
    fn reads_own_process_and_skips_dead_ones() {
        let t = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let start = proc_start_ticks(me)
            .map(|s| format!(",\"procStart\":\"{s}\""))
            .unwrap_or_default();
        std::fs::write(
            t.path().join(format!("{me}.json")),
            format!(r#"{{"pid":{me},"sessionId":"s1","cwd":"/w","startedAt":1795348783929{start},"name":"n","status":"waiting","waitingFor":"input needed","tmux":"0:@0.%6"}}"#),
        )
        .unwrap();
        std::fs::write(
            t.path().join("999999999.json"),
            r#"{"pid":999999999,"sessionId":"dead","cwd":"/w","startedAt":1}"#,
        )
        .unwrap();
        std::fs::write(t.path().join("x.key"), "secret").unwrap();
        let procs = ProcTable::snapshot();
        let s = live_sessions("default", t.path(), &procs);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].session_id, "s1");
        assert_eq!(s[0].status, SessionStatus::Waiting("input needed".into()));
        assert_eq!(s[0].tmux.as_deref(), Some("0:@0.%6"));
    }
}
