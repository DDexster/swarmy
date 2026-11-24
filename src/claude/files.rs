//! The transcript files of a seat and the metadata of subagents.
//!
//! ```text
//! projects/<encoded cwd>/<session>.jsonl                                main thread
//! projects/<encoded cwd>/<session>/subagents/agent-<id>.jsonl           subagent (+ .meta.json)
//! projects/<encoded cwd>/<session>/subagents/workflows/<run>/agent-<id>.jsonl   workflow agent
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::seats::Seat;
use crate::core::harness::Harness;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FileKind {
    Main,
    Subagent,
    Workflow(String),
}

impl FileKind {
    /// Every tag `tag` returns; the index's `CHECK` on `files.kind` and `agents.kind` is
    /// generated from it.
    pub const TAGS: [&'static str; 3] = ["main", "subagent", "workflow"];

    pub fn tag(&self) -> &'static str {
        match self {
            FileKind::Main => Self::TAGS[0],
            FileKind::Subagent => Self::TAGS[1],
            FileKind::Workflow(_) => Self::TAGS[2],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptFile {
    pub path: PathBuf,
    pub seat: super::seats::SeatId,
    pub session_id: String,
    /// Empty for the main thread.
    pub agent_id: String,
    pub kind: FileKind,
    /// Which harness wrote this file.
    pub harness: Harness,
    /// Session working directory supplied out-of-band (used by harnesses like `agy` whose
    /// transcript lines do not repeat `cwd`).
    pub default_cwd: Option<String>,
    /// Session title supplied out-of-band (e.g. `agy`'s `conversation_summaries.db`).
    pub session_title: Option<String>,
    /// Agent metadata supplied out-of-band (instead of Claude Code's `agent-*.meta.json`).
    pub inline_meta: Option<AgentMeta>,
}

impl TranscriptFile {
    pub fn meta_path(&self) -> Option<PathBuf> {
        (self.harness == Harness::Claude && self.kind != FileKind::Main)
            .then(|| self.path.with_extension("meta.json"))
    }
}

fn jsonl_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    v.sort();
    v
}

fn agent_id_of(p: &Path) -> Option<String> {
    let stem = p.file_stem()?.to_string_lossy();
    stem.strip_prefix("agent-").map(str::to_string)
}

fn claude_file(
    path: PathBuf,
    seat: &Seat,
    session_id: String,
    agent_id: String,
    kind: FileKind,
) -> TranscriptFile {
    TranscriptFile {
        path,
        seat: seat.id.clone(),
        session_id,
        agent_id,
        kind,
        harness: Harness::Claude,
        default_cwd: None,
        session_title: None,
        inline_meta: None,
    }
}

pub fn list(seat: &Seat) -> Vec<TranscriptFile> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(seat.projects_dir()) else {
        return out;
    };
    for proj in projects.flatten() {
        let pdir = proj.path();
        if !pdir.is_dir() {
            continue;
        }
        // Main transcripts, and session dirs (which may exist without a main file).
        let mut sessions: Vec<String> = Vec::new();
        for f in jsonl_in(&pdir) {
            let sid = f
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            sessions.push(sid.clone());
            out.push(claude_file(f, seat, sid, String::new(), FileKind::Main));
        }
        let Ok(entries) = std::fs::read_dir(&pdir) else {
            continue;
        };
        for e in entries.flatten() {
            let sdir = e.path();
            if !sdir.is_dir() {
                continue;
            }
            let sid = e.file_name().to_string_lossy().into_owned();
            let sub = sdir.join("subagents");
            for f in jsonl_in(&sub) {
                if let Some(aid) = agent_id_of(&f) {
                    out.push(claude_file(f, seat, sid.clone(), aid, FileKind::Subagent));
                }
            }
            if let Ok(runs) = std::fs::read_dir(sub.join("workflows")) {
                for run in runs.flatten() {
                    let run_id = run.file_name().to_string_lossy().into_owned();
                    for f in jsonl_in(&run.path()) {
                        if let Some(aid) = agent_id_of(&f) {
                            out.push(claude_file(
                                f,
                                seat,
                                sid.clone(),
                                aid,
                                FileKind::Workflow(run_id.clone()),
                            ));
                        }
                    }
                }
            }
        }
    }
    out
}

/// The files of one session (main thread, subagents, workflow agents) wherever its project dir is.
pub fn list_session(seat: &Seat, session_id: &str) -> Vec<TranscriptFile> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(seat.projects_dir()) else {
        return out;
    };
    for proj in projects.flatten() {
        let pdir = proj.path();
        let main = pdir.join(format!("{session_id}.jsonl"));
        let sdir = pdir.join(session_id);
        if !main.exists() && !sdir.is_dir() {
            continue;
        }
        if main.exists() {
            out.push(claude_file(
                main,
                seat,
                session_id.into(),
                String::new(),
                FileKind::Main,
            ));
        }
        let sub = sdir.join("subagents");
        for f in jsonl_in(&sub) {
            if let Some(aid) = agent_id_of(&f) {
                out.push(claude_file(
                    f,
                    seat,
                    session_id.into(),
                    aid,
                    FileKind::Subagent,
                ));
            }
        }
        if let Ok(runs) = std::fs::read_dir(sub.join("workflows")) {
            for run in runs.flatten() {
                let run_id = run.file_name().to_string_lossy().into_owned();
                for f in jsonl_in(&run.path()) {
                    if let Some(aid) = agent_id_of(&f) {
                        out.push(claude_file(
                            f,
                            seat,
                            session_id.into(),
                            aid,
                            FileKind::Workflow(run_id.clone()),
                        ));
                    }
                }
            }
        }
    }
    out
}

/// `agent-<id>.meta.json`, written by Claude Code when it spawns a subagent.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentMeta {
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub tool_use_id: Option<String>,
    pub parent_agent_id: Option<String>,
    pub spawn_depth: Option<u32>,
    pub request_shape: Option<String>,
    /// As requested (`opus`, `fable`, a full id…).
    pub model: Option<String>,
    pub effort: Option<String>,
    pub picked_effort_model: Option<String>,
    pub finished: bool,
}

pub fn read_meta(path: &Path) -> Option<AgentMeta> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// `custom-title.json` in a session dir.
pub fn read_custom_title(session_dir: &Path) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(session_dir.join("custom-title.json")).ok()?)
            .ok()?;
    v.get("customTitle")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_main_subagent_and_workflow_files() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("projects/-w-shop");
        std::fs::create_dir_all(p.join("s1/subagents/workflows/wf_1")).unwrap();
        std::fs::create_dir_all(p.join("s2/tool-results")).unwrap();
        std::fs::write(p.join("s1.jsonl"), "").unwrap();
        std::fs::write(p.join("s1/subagents/agent-a1.jsonl"), "").unwrap();
        std::fs::write(
            p.join("s1/subagents/agent-a1.meta.json"),
            r#"{"agentType":"general-purpose","model":"opus","finished":true,"spawnDepth":1}"#,
        )
        .unwrap();
        std::fs::write(p.join("s1/subagents/workflows/wf_1/agent-w1.jsonl"), "").unwrap();
        let seat = Seat::new(
            "default",
            t.path().into(),
            t.path().join(".claude.json"),
            true,
        );
        let files = list(&seat);
        let kinds: Vec<_> = files
            .iter()
            .map(|f| (f.session_id.as_str(), f.agent_id.as_str(), f.kind.tag()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("s1", "", "main"),
                ("s1", "a1", "subagent"),
                ("s1", "w1", "workflow")
            ]
        );
        let meta = read_meta(&files[1].meta_path().unwrap()).unwrap();
        assert_eq!(meta.model.as_deref(), Some("opus"));
        assert!(meta.finished);
        assert_eq!(meta.spawn_depth, Some(1));
    }
}
