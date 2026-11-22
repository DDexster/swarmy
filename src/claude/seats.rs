//! Discovery of Claude Code config dirs ("seats").
//!
//! Claude Code keeps everything in `~/.claude` unless `CLAUDE_CONFIG_DIR` points elsewhere, which
//! people do with shell aliases to run several accounts side by side. A seat is found by any of:
//! the default dir, `~/.claude-*` siblings that look like config dirs, `CLAUDE_CONFIG_DIR` of this
//! process or of a running `claude`, and the `seats` list of `config.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Seat {
    /// Short name: `default` for `~/.claude`, `work` for `~/.claude-work`, else the dir name.
    pub label: String,
    pub dir: PathBuf,
    /// `~/.claude.json` for the default dir, `<dir>/.claude.json` for the others.
    pub claude_json: PathBuf,
    pub is_default: bool,
    pub found_by: Vec<&'static str>,
}

impl Seat {
    pub fn projects_dir(&self) -> PathBuf {
        self.dir.join("projects")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.dir.join("sessions")
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.dir.join(".credentials.json")
    }

    pub fn history_file(&self) -> PathBuf {
        self.dir.join("history.jsonl")
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
        let key = std::fs::canonicalize(&p).unwrap_or(p);
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

    let canon_default = std::fs::canonicalize(&default).unwrap_or(default);
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
                label,
                dir,
                claude_json,
                is_default,
                found_by,
            }
        })
        .collect();
    // Default first, then by label; labels made unique.
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
    }

    #[test]
    fn labels() {
        let h = Path::new("/h");
        assert_eq!(label_for(Path::new("/h/.claude"), h), "default");
        assert_eq!(label_for(Path::new("/h/.claude-work"), h), "work");
        assert_eq!(label_for(Path::new("/srv/agents"), h), "agents");
    }
}
