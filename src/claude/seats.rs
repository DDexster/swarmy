//! Discovery of Claude Code config dirs ("seats").
//!
//! Claude Code keeps everything in `~/.claude` unless `CLAUDE_CONFIG_DIR` points elsewhere, which
//! people do with shell aliases to run several accounts side by side. A seat is found by any of:
//! the default dir, `~/.claude-*` siblings that look like config dirs, `CLAUDE_CONFIG_DIR` of this
//! process or of a running `claude`, and the `seats` list of `config.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub use crate::core::harness::Harness;
pub use crate::core::ids::SeatId;

#[derive(Debug, Clone, Serialize)]
pub struct Seat {
    /// The seat's identity, from its canonical dir: what the index and the vault store.
    pub id: SeatId,
    /// Short name for display only: `default` for `~/.claude`, `work` for `~/.claude-work`, else
    /// the dir name, or the `label` of `config.toml`; may change between runs.
    pub label: String,
    pub dir: PathBuf,
    /// `~/.claude.json` for the default Claude dir, `<dir>/.claude.json` for other Claude dirs;
    /// empty for non-Claude harnesses.
    pub claude_json: PathBuf,
    pub is_default: bool,
    pub found_by: Vec<&'static str>,
    /// Which agent harness owns this directory.
    #[serde(default)]
    pub harness: Harness,
}

/// The id of a config dir: [`SeatId::of_canonical_dir`] of its canonical path (the path as
/// given when it cannot be resolved, e.g. a dir that is gone).
pub fn seat_id(dir: &Path) -> SeatId {
    let canon = crate::fsutil::canonical(dir).unwrap_or_else(|_| dir.to_path_buf());
    SeatId::of_canonical_dir(&canon)
}

/// Where the `.claude.json` of config dir `dir` lives: `~/.claude.json` for the default dir
/// (`~/.claude`), `<dir>/.claude.json` for any other (the rule [`discover`] follows).
pub fn claude_json_of(dir: &Path, home: &Path) -> PathBuf {
    let canon = |p: &Path| crate::fsutil::canonical(p).unwrap_or_else(|_| p.to_path_buf());
    if canon(dir) == canon(&home.join(".claude")) {
        home.join(".claude.json")
    } else {
        dir.join(".claude.json")
    }
}

impl Seat {
    /// A Claude Code seat of `dir`, its id derived from the path.
    pub fn new(
        label: impl Into<String>,
        dir: PathBuf,
        claude_json: PathBuf,
        is_default: bool,
    ) -> Seat {
        Seat {
            id: seat_id(&dir),
            label: label.into(),
            dir,
            claude_json,
            is_default,
            found_by: Vec::new(),
            harness: Harness::Claude,
        }
    }

    /// An Antigravity (`agy`) seat of `dir`, its id derived from the path.
    pub fn agy(label: impl Into<String>, dir: PathBuf) -> Seat {
        Seat {
            id: seat_id(&dir),
            label: label.into(),
            dir,
            claude_json: PathBuf::new(),
            is_default: false,
            found_by: Vec::new(),
            harness: Harness::Agy,
        }
    }

    /// A Codex seat of `dir` (a Codex home), its id derived from the path.
    pub fn codex(label: impl Into<String>, dir: PathBuf) -> Seat {
        Seat {
            id: seat_id(&dir),
            label: label.into(),
            dir,
            claude_json: PathBuf::new(),
            is_default: false,
            found_by: Vec::new(),
            harness: Harness::Codex,
        }
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.dir.join("projects")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.dir.join("sessions")
    }

    /// Directories holding this seat's transcripts on disk (for `SYSTEM` disk usage).
    pub fn disk_dirs(&self) -> Vec<PathBuf> {
        match self.harness {
            Harness::Claude => vec![self.projects_dir()],
            Harness::Agy => vec![self.dir.join("brain"), self.dir.join("conversations")],
            Harness::Codex => vec![
                self.dir.join("sessions"),
                self.dir.join("archived_sessions"),
            ],
        }
    }

    pub fn credentials_file(&self) -> PathBuf {
        match self.harness {
            Harness::Claude => self.dir.join(".credentials.json"),
            Harness::Agy => self.dir.join("antigravity-oauth-token"),
            Harness::Codex => self.dir.join("auth.json"),
        }
    }

    /// Days Claude Code keeps this seat's transcripts: `cleanupPeriodDays` of its
    /// `settings.json` (at least 1), `None` when not set (Claude Code's default applies,
    /// `core::storage::DEFAULT_RETENTION_DAYS`). Harnesses without automatic transcript deletion
    /// return `Ok(None)`. A file that exists but cannot be read or parsed is an error, never "not
    /// set".
    pub fn cleanup_period_days(&self) -> Result<Option<u32>, crate::errors::ConfigError> {
        if !self.harness.has_transcript_retention() {
            return Ok(None);
        }
        let path = self.dir.join("settings.json");
        let damaged = |message: String| crate::errors::ConfigError::SeatSettings {
            path: path.clone(),
            message,
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(damaged(e.to_string())),
        };
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| damaged(e.to_string()))?;
        let Some(days) = v
            .get("cleanupPeriodDays")
            .and_then(serde_json::Value::as_f64)
        else {
            return Ok(None);
        };
        Ok((days.is_finite() && days >= 1.0).then(|| days.min(f64::from(u32::MAX)) as u32))
    }
}

/// A config dir is recognised by the files Claude Code creates in it.
fn looks_like_config_dir(p: &Path) -> bool {
    p.is_dir()
        && [
            "projects",
            "sessions",
            ".credentials.json",
            "settings.json",
            "history.jsonl",
        ]
        .iter()
        .any(|f| p.join(f).exists())
}

pub fn label_for(dir: &Path, home: &Path) -> String {
    if dir == home.join(".claude") {
        return "default".into();
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    for prefix in [".claude-", ".claude_", "claude-", "claude_"] {
        if let Some(rest) = name.strip_prefix(prefix)
            && !rest.is_empty()
        {
            return rest.to_string();
        }
    }
    name.trim_start_matches('.').to_string()
}

pub struct Candidates<'a> {
    pub home: &'a Path,
    /// `CLAUDE_CONFIG_DIR` values seen in this process and in running `claude` processes.
    pub env_dirs: Vec<PathBuf>,
    /// From `config.toml`, with optional labels.
    pub configured: Vec<(PathBuf, Option<String>)>,
}

pub fn discover(c: &Candidates) -> Vec<Seat> {
    let mut found: BTreeMap<PathBuf, (Vec<&'static str>, Option<String>)> = BTreeMap::new();
    let mut add = |p: PathBuf, by: &'static str, label: Option<String>| {
        let key = crate::fsutil::canonical(&p).unwrap_or(p);
        let e = found.entry(key).or_default();
        if !e.0.contains(&by) {
            e.0.push(by);
        }
        if label.is_some() {
            e.1 = label;
        }
    };

    let default = c.home.join(".claude");
    if looks_like_config_dir(&default) {
        add(default.clone(), "default", None);
    }
    if let Ok(rd) = std::fs::read_dir(c.home) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if (name.starts_with(".claude-") || name.starts_with(".claude_"))
                && looks_like_config_dir(&e.path())
            {
                add(e.path(), "home scan", None);
            }
        }
    }
    for d in &c.env_dirs {
        if looks_like_config_dir(d) {
            add(d.clone(), "CLAUDE_CONFIG_DIR", None);
        }
    }
    for (d, label) in &c.configured {
        if d.is_dir() {
            add(d.clone(), "config.toml", label.clone());
        }
    }

    let canon_default = crate::fsutil::canonical(&default).unwrap_or(default);
    let mut seats: Vec<Seat> = found
        .into_iter()
        .map(|(dir, (found_by, label))| {
            let is_default = dir == canon_default;
            let claude_json = if is_default {
                c.home.join(".claude.json")
            } else {
                dir.join(".claude.json")
            };
            let label = label.unwrap_or_else(|| {
                if is_default {
                    "default".into()
                } else {
                    label_for(&dir, c.home)
                }
            });
            Seat {
                // `dir` is canonical already (the key of `found`).
                id: SeatId::of_canonical_dir(&dir),
                label,
                dir,
                claude_json,
                is_default,
                found_by,
                harness: Harness::Claude,
            }
        })
        .collect();
    // Default first, then by label; labels made unique (for display: ids never change).
    seats.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.label.cmp(&b.label)));
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for s in &mut seats {
        let n = seen.entry(s.label.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            s.label = format!("{}-{}", s.label, n);
        }
    }
    seats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_default_siblings_env_and_config() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        for d in [".claude", ".claude-work", ".claude-empty", "custom/cc"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::create_dir_all(home.join(".claude/projects")).unwrap();
        std::fs::create_dir_all(home.join(".claude-work/sessions")).unwrap();
        std::fs::write(home.join("custom/cc/settings.json"), "{}").unwrap();
        let seats = discover(&Candidates {
            home,
            env_dirs: vec![home.join("custom/cc")],
            configured: vec![(home.join(".claude-work"), Some("job".into()))],
        });
        let labels: Vec<_> = seats.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["default", "cc", "job"], "empty dirs are not seats");
        assert_eq!(seats[0].claude_json, home.join(".claude.json"));
        assert!(seats[2].claude_json.ends_with(".claude-work/.claude.json"));
        assert_eq!(seats[2].found_by, ["home scan", "config.toml"]);
        // Transcript retention: `cleanupPeriodDays`, else Claude Code's default.
        assert_eq!(
            seats[1].cleanup_period_days().unwrap(),
            None,
            "settings.json without it"
        );
        assert_eq!(
            seats[0].cleanup_period_days().unwrap(),
            None,
            "no settings.json"
        );
        std::fs::write(
            home.join("custom/cc/settings.json"),
            r#"{"cleanupPeriodDays": 45}"#,
        )
        .unwrap();
        assert_eq!(seats[1].cleanup_period_days().unwrap(), Some(45));
        std::fs::write(
            home.join("custom/cc/settings.json"),
            r#"{"cleanupPeriodDays": 0}"#,
        )
        .unwrap();
        assert_eq!(
            seats[1].cleanup_period_days().unwrap(),
            None,
            "below Claude Code's minimum"
        );
        // A damaged file is no "not set": the retention is unknown, and said so (coded).
        std::fs::write(
            home.join("custom/cc/settings.json"),
            r#"{"cleanupPeriodDays": 7"#,
        )
        .unwrap();
        let e = anyhow::Error::from(seats[1].cleanup_period_days().unwrap_err());
        assert_eq!(crate::errors::code_of(&e), Some("config-seat-settings"));
    }

    /// Review H3: a seat's id comes from its dir, never from its label. Discovering again with
    /// another dir of the same name (which takes the label and pushes this one to `work-2`) or
    /// with a `label` in config.toml changes no id.
    #[test]
    fn ids_survive_relabelling() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        for d in [
            ".claude-work/projects",
            "zz/claude-work/projects",
            "aa/.claude-work/projects",
        ] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        let ids = |seats: &[Seat]| -> BTreeMap<PathBuf, (SeatId, String)> {
            seats
                .iter()
                .map(|s| (s.dir.clone(), (s.id.clone(), s.label.clone())))
                .collect()
        };
        let first = discover(&Candidates {
            home,
            env_dirs: vec![home.join("zz/claude-work")],
            configured: vec![],
        });
        let before = ids(&first);
        assert_eq!(first.len(), 2);
        let second = discover(&Candidates {
            home,
            env_dirs: vec![home.join("zz/claude-work"), home.join("aa/.claude-work")],
            configured: vec![(home.join(".claude-work"), Some("job".into()))],
        });
        let after = ids(&second);
        assert_eq!(second.len(), 3);
        let mut relabelled = 0;
        for (dir, (id, label)) in &before {
            let (id2, label2) = &after[dir];
            assert_eq!(id, id2, "{} keeps its id", dir.display());
            relabelled += usize::from(label != label2);
            assert_eq!(*id, seat_id(dir));
        }
        assert!(
            relabelled >= 1,
            "the labels did move: {before:?} → {after:?}"
        );
        let unique: std::collections::BTreeSet<_> = after.values().map(|v| &v.0).collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn labels() {
        let h = Path::new("/h");
        assert_eq!(label_for(Path::new("/h/.claude"), h), "default");
        assert_eq!(label_for(Path::new("/h/.claude-work"), h), "work");
        assert_eq!(label_for(Path::new("/srv/agents"), h), "agents");
    }
}
