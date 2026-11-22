//! `config.toml` and the tool's own directories.
//!
//! Everything is optional: with no config file the tool discovers seats on its own.

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::claude::project::Alias;
use crate::core::pricing::{PriceBook, PriceEntry, PriceSource, Rates};

#[derive(Debug, Clone)]
pub struct Dirs {
    /// Index database, vault metadata, backups.
    pub data: PathBuf,
    /// `config.toml`.
    pub config: PathBuf,
    pub home: PathBuf,
}

impl Dirs {
    pub fn resolve() -> anyhow::Result<Dirs> {
        let base = directories::BaseDirs::new().context("cannot determine the home directory")?;
        let home = base.home_dir().to_path_buf();
        let pd = directories::ProjectDirs::from("", "", "swarmy")
            .context("cannot determine data directories")?;
        let data = std::env::var_os("SWARMY_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| pd.data_dir().to_path_buf());
        let config = std::env::var_os("SWARMY_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| pd.config_dir().to_path_buf());
        // The tool was called agentmana before: move its data and config (index, vault of saved
        // logins, config.toml) over once. Never while the old program runs (it would recreate
        // and write its dirs), and never over anything already in the new place.
        if let Some(old) = directories::ProjectDirs::from("", "", "agentmana")
            && (old.data_dir().exists() || old.config_dir().exists())
            && !old_program_running()
        {
            for (from, to) in [(old.data_dir(), &data), (old.config_dir(), &config)] {
                migrate_dir(from, to);
            }
        }
        Ok(Dirs { data, config, home })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    pub fn index_file(&self) -> PathBuf {
        self.data.join("index.sqlite")
    }

    pub fn vault_dir(&self) -> PathBuf {
        self.data.join("vault")
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Extra config dirs: `[[seat]] path = "~/claude-work"  label = "work"`.
    pub seat: Vec<SeatConf>,
    /// Project aliases: `[[project]] match = "/old/path"  name = "shop"`.
    pub project: Vec<Alias>,
    /// Price overrides: `[[price]] model = "claude-opus-5-5"  from = "2026-12-01"  input = 4 …`.
    pub price: Vec<PriceConf>,
    /// Display names for accounts: `[accounts] "<account uuid>" = "work"`.
    pub accounts: std::collections::BTreeMap<String, String>,
    /// The Claude Code status line (`swarmy statusline`).
    pub statusline: StatuslineConf,
    /// Monthly subscription price per account label or uuid, for the payback figure.
    pub subscription: std::collections::BTreeMap<String, f64>,
    /// Minutes without transcript activity after which an unfinished agent counts as stalled.
    pub stall_minutes: Option<u64>,
    pub theme: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SeatConf {
    pub path: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PriceConf {
    /// Exact id or a prefix ending in `*`.
    pub model: String,
    pub from: Option<NaiveDate>,
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: Option<f64>,
    pub cache_write_1h: Option<f64>,
    pub cache_read: Option<f64>,
    pub web_search: Option<f64>,
    pub fast_multiplier: Option<f64>,
}

impl Config {
    pub fn load(dirs: &Dirs) -> anyhow::Result<Config> {
        let p = dirs.config_file();
        match std::fs::read_to_string(&p) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("invalid {}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", p.display())),
        }
    }

    pub fn stall_minutes(&self) -> u64 {
        self.stall_minutes.unwrap_or(20)
    }

    pub fn seat_paths(&self, home: &Path) -> Vec<(PathBuf, Option<String>)> {
        self.seat
            .iter()
            .map(|s| (expand_home(&s.path, home), s.label.clone()))
            .collect()
    }

    pub fn apply_prices(&self, book: &mut PriceBook) {
        for p in &self.price {
            book.push(PriceEntry {
                model: p.model.clone(),
                from: p
                    .from
                    .unwrap_or(NaiveDate::from_ymd_opt(2023, 1, 1).unwrap()),
                rates: Rates {
                    input: p.input,
                    output: p.output,
                    cache_write_5m: p.cache_write_5m.unwrap_or(p.input * 1.25),
                    cache_write_1h: p.cache_write_1h.unwrap_or(p.input * 2.0),
                    cache_read: p.cache_read.unwrap_or(p.input * 0.1),
                    web_search: p.web_search.unwrap_or(0.01),
                    fast_multiplier: p.fast_multiplier,
                    long_context: None,
                },
                source: PriceSource::User,
            });
        }
    }
}

pub fn expand_home(p: &str, home: &Path) -> PathBuf {
    if p == "~" {
        home.to_path_buf()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_config() {
        let c: Config = toml::from_str(
            r#"
            stall_minutes = 30
            [[seat]]
            path = "~/cc-work"
            label = "work"
            [[project]]
            match = "/home/user/work/old"
            name = "shop"
            [[price]]
            model = "claude-opus-5-5"
            from = "2026-12-01"
            input = 3
            output = 15
            [accounts]
            "acc-1" = "personal"
            [subscription]
            personal = 200
            "#,
        )
        .unwrap();
        assert_eq!(c.stall_minutes(), 30);
        assert_eq!(
            c.seat_paths(Path::new("/h"))[0].0,
            PathBuf::from("/h/cc-work")
        );
        let mut b = PriceBook::default();
        c.apply_prices(&mut b);
        assert_eq!(b.entries()[0].rates.cache_write_1h, 6.0);
        assert!(
            toml::from_str::<Config>("bogus = 1").is_err(),
            "typos are reported"
        );
    }
}

fn old_program_running() -> bool {
    let Ok(mut sys) = sysinfo::System::new() else {
        // Cannot tell: assume it runs, and migrate another time.
        return true;
    };
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    sys.processes()
        .values()
        .any(|p| p.name().to_string_lossy().starts_with("agentmana"))
}

/// Moves `from` into `to` entry by entry; entries that already exist in `to` stay where they are.
fn migrate_dir(from: &std::path::Path, to: &std::path::Path) {
    if !from.is_dir() || from == to {
        return;
    }
    let _ = std::fs::create_dir_all(to);
    if let Ok(entries) = std::fs::read_dir(from) {
        for e in entries.flatten() {
            let dest = to.join(e.file_name());
            if dest.exists() {
                if e.path().is_dir() && dest.is_dir() {
                    migrate_dir(&e.path(), &dest);
                }
                continue;
            }
            let _ = std::fs::rename(e.path(), &dest);
        }
    }
    // Remove the old dir only when everything moved.
    let _ = std::fs::remove_dir(from);
}

/// `[statusline]` in config.toml.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatuslineConf {
    /// Segments in display order: attention, agents, week, five_hour, context, cost, burn.
    pub segments: Vec<String>,
    /// 1, or 2 for a second line with the model, effort, branch and session name.
    pub lines: u8,
    /// Seconds between refreshes of an idle session's line (`swarmy setup` writes it into
    /// Claude Code's `refreshInterval`); 0 = only on Claude Code's own events.
    pub refresh_seconds: u32,
    /// The dashboard's summary counts while it is younger than this.
    pub fresh_seconds: u32,
    /// Columns left free on the right for Claude Code's own notifications.
    pub reserve: u16,
    /// Hide session and project names (also `SWARMY_MASK=1`).
    pub mask: bool,
}

impl Default for StatuslineConf {
    fn default() -> Self {
        StatuslineConf {
            segments: [
                "attention",
                "agents",
                "week",
                "five_hour",
                "context",
                "cost",
                "burn",
            ]
            .map(String::from)
            .to_vec(),
            lines: 1,
            refresh_seconds: 10,
            fresh_seconds: 15,
            reserve: 0,
            mask: false,
        }
    }
}
