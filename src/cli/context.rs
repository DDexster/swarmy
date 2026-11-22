//! Everything a command needs: dirs, config, seats, prices, a fresh index.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::claude::config_json;
use crate::claude::procs::ProcTable;
use crate::claude::project::Resolver;
use crate::claude::seats::{self, Candidates, Seat};
use crate::config::{Config, Dirs};
use crate::core::limits::{Reading, WindowKind};
use crate::core::period::Span;
use crate::core::pricing::PriceBook;
use crate::index::{Index, Progress, RefreshStats, SeatInfo};
use crate::ui::fmt;
use crate::ui::style::Painter;

pub struct Ctx {
    pub dirs: Dirs,
    pub config: Config,
    pub seats: Vec<Seat>,
    pub seat_info: Vec<SeatInfo>,
    pub prices: PriceBook,
    pub procs: ProcTable,
    pub painter: Painter,
    pub mask: bool,
    pub now: DateTime<Utc>,
    /// Saved logins by account: names and plan tiers for accounts not in any seat right now.
    pub login_labels: std::collections::BTreeMap<String, String>,
    pub login_tiers: std::collections::BTreeMap<String, String>,
}

impl Ctx {
    pub fn load(painter: Painter, mask: bool) -> anyhow::Result<Ctx> {
        let dirs = Dirs::resolve()?;
        let config = Config::load(&dirs)?;
        let procs = ProcTable::snapshot();
        // `SWARMY_ISOLATED=1` (tests, demos): only dirs under $HOME and the config count, not the
        // config dirs of running `claude` processes, which belong to the real user.
        let isolated = std::env::var_os("SWARMY_ISOLATED").is_some();
        let mut env_dirs: Vec<PathBuf> = procs
            .claude
            .iter()
            .filter(|_| !isolated)
            .filter_map(|p| p.config_dir.clone())
            .collect();
        if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
            env_dirs.push(PathBuf::from(d));
        }
        let seats = seats::discover(&Candidates {
            home: &dirs.home,
            env_dirs,
            configured: config.seat_paths(&dirs.home),
        });
        let mut prices = PriceBook::builtin();
        let mut seat_info = Vec::new();
        for s in &seats {
            let (info, costs) = seat_info_of(s, &dirs.data);
            if let Some(costs) = &costs {
                prices.merge_claude_code_cache(&s.label, costs);
            }
            seat_info.push(info);
        }
        config.apply_prices(&mut prices);
        let mut ctx = Ctx {
            dirs,
            config,
            seats,
            seat_info,
            prices,
            procs,
            painter,
            mask,
            now: Utc::now(),
            login_labels: Default::default(),
            login_tiers: Default::default(),
        };
        ctx.drop_unconfirmed();
        Ok(ctx)
    }

    /// Seats whose current login is not confirmed lose the stale profile, so they are not merged
    /// into another account and do not borrow its limit reading.
    pub fn drop_unconfirmed(&mut self) {
        let Ok(mut v) = crate::vault::Vault::open(&self.dirs.vault_dir()) else {
            return;
        };
        // Names and plan tiers of saved logins, for accounts that are in no seat right now.
        self.login_labels.clear();
        self.login_tiers.clear();
        for l in v.logins() {
            if let Some(a) = &l.account_uuid {
                self.login_labels
                    .entry(a.clone())
                    .or_insert_with(|| l.label.clone());
                if let Some(t) = l.plan.clone().or_else(|| l.meta.rate_limit_tier.clone()) {
                    self.login_tiers.insert(a.clone(), t);
                }
            }
        }
        let Ok(states) = v.seat_states(&self.seats) else {
            return;
        };
        for si in &mut self.seat_info {
            if states
                .get(&si.label)
                .is_some_and(|st| st.unverified_since.is_some())
            {
                si.account = None;
                si.utilization = None;
            }
        }
    }

    /// A context over the demo world only: no discovery, its own index and vault.
    pub fn demo(painter: Painter, seats: Vec<Seat>, root: &std::path::Path) -> anyhow::Result<Ctx> {
        let base = Dirs::resolve()?;
        let dirs = Dirs {
            data: root.join("swarmy-data"),
            config: root.join("swarmy-config"),
            home: base.home,
        };
        let mut prices = PriceBook::builtin();
        let mut seat_info = Vec::new();
        for s in &seats {
            let cj = config_json::read(&s.claude_json).unwrap_or_default();
            if let Some(costs) = &cj.model_costs {
                prices.merge_claude_code_cache(&s.label, costs);
            }
            seat_info.push(SeatInfo {
                label: s.label.clone(),
                account: cj.account,
                utilization: cj.utilization,
            });
        }
        let mut ctx = Ctx {
            dirs,
            config: Config::default(),
            seats,
            seat_info,
            prices,
            procs: ProcTable::snapshot(),
            painter,
            mask: false,
            now: Utc::now(),
            login_labels: Default::default(),
            login_tiers: Default::default(),
        };
        ctx.drop_unconfirmed();
        Ok(ctx)
    }

    /// Opens the index and reads whatever changed since the last run. The first run shows
    /// progress on stderr.
    pub fn index(&self) -> anyhow::Result<(Index, RefreshStats)> {
        let mut idx = Index::open(&self.dirs.index_file())?;
        let mut resolver = Resolver::new(self.config.project.clone());
        let tty = std::io::stderr().is_terminal();
        let mut last_draw = std::time::Instant::now();
        let mut shown = false;
        let mut progress = |p: Progress| {
            if !tty || p.files_total < 50 {
                return;
            }
            if last_draw.elapsed().as_millis() < 100 && p.files_done < p.files_total {
                return;
            }
            last_draw = std::time::Instant::now();
            shown = true;
            let frac = if p.bytes_total > 0 {
                p.bytes_done as f64 / p.bytes_total as f64
            } else {
                1.0
            };
            eprint!(
                "\r\x1b[2K[ .. ] indexing {} transcripts {} {:>3.0}%  {:.0} MB",
                fmt::count(p.files_total as u64),
                fmt::gauge(frac, 24),
                frac * 100.0,
                p.bytes_done as f64 / 1e6
            );
            let _ = std::io::stderr().flush();
        };
        let stats = idx.refresh(
            &self.seats,
            &self.seat_info,
            &mut resolver,
            &self.prices,
            &mut progress,
        )?;
        if shown {
            eprintln!(
                "\r\x1b[2K[ OK ] indexed {} transcripts in {:.1}s",
                fmt::count(stats.files_read as u64),
                stats.elapsed.as_secs_f64()
            );
        }
        Ok((idx, stats))
    }

    /// Display name of an account: the config label, else the email (masked on request), else
    /// the seats it is logged into, else a short uuid.
    pub fn account_name(&self, uuid: &str) -> String {
        if let Some(l) = self.config.accounts.get(uuid) {
            return l.clone();
        }
        // A saved login's label names its account best: seat names would mix the two ideas.
        if let Some(l) = self.login_labels.get(uuid) {
            return l.clone();
        }
        let seats: Vec<&str> = self
            .seat_info
            .iter()
            .filter(|s| s.account.as_ref().is_some_and(|a| a.account_uuid == uuid))
            .map(|s| s.label.as_str())
            .collect();
        if !seats.is_empty() {
            return format!("account in {}", seats.join("+"));
        }
        if !self.mask
            && let Some(e) = self
                .seat_info
                .iter()
                .filter_map(|s| s.account.as_ref())
                .find(|a| a.account_uuid == uuid)
                .and_then(|a| a.email.clone())
        {
            return e;
        }
        format!("acct-{}", &uuid[..uuid.len().min(8)])
    }

    pub fn email(&self, uuid: &str) -> Option<String> {
        if self.mask {
            return None;
        }
        self.seat_info
            .iter()
            .filter_map(|s| s.account.as_ref())
            .find(|a| a.account_uuid == uuid)
            .and_then(|a| a.email.clone())
    }

    /// The freshest utilization reading of every account Claude Code has cached.
    pub fn latest_readings(&self) -> Vec<(String, Vec<Reading>)> {
        let mut best: std::collections::BTreeMap<String, &config_json::Utilization> =
            std::collections::BTreeMap::new();
        for si in &self.seat_info {
            if let Some(u) = &si.utilization {
                let e = best.entry(u.account_uuid.clone()).or_insert(u);
                if u.fetched_at > e.fetched_at {
                    *e = u;
                }
            }
        }
        best.into_iter()
            .map(|(k, u)| (k, u.readings.clone()))
            .collect()
    }

    /// The current weekly window of the most recently active account, for `--period week`.
    pub fn limit_week(&self, account: Option<&str>) -> Option<Span> {
        let readings = self.latest_readings();
        let pick = match account {
            Some(a) => readings.iter().find(|(u, _)| u == a),
            None => readings
                .iter()
                .max_by_key(|(_, r)| r.iter().map(|x| x.fetched_at).max()),
        }?;
        let r = pick.1.iter().find(|r| r.kind == WindowKind::SevenDay)?;
        let mut w = r.window();
        while w.end <= self.now {
            // The cached reading is from a previous window: roll forward.
            w = Span {
                start: w.end,
                end: w.end + chrono::Duration::days(7),
            };
        }
        Some(Span {
            start: w.start,
            end: w.end.min(self.now + chrono::Duration::seconds(1)),
        })
    }

    /// `/home/user/x` → `~/x`, and just the last component when masking.
    pub fn show_path(&self, p: &str) -> String {
        let home = self.dirs.home.to_string_lossy();
        if self.mask {
            return std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        match p.strip_prefix(home.as_ref()) {
            Some(rest) => format!("~{rest}"),
            None => p.to_string(),
        }
    }
}

/// What a seat says about itself: its account from `.claude.json`, and its limit readings — the
/// status line's when newer than Claude Code's cached ones (see `claude::statusline`).
pub fn seat_info_of(s: &Seat, data_dir: &std::path::Path) -> (SeatInfo, Option<serde_json::Value>) {
    let cj = config_json::read(&s.claude_json).unwrap_or_default();
    let live =
        crate::claude::statusline::read(&crate::claude::statusline::file_for(data_dir, &s.dir));
    let account = cj.account.as_ref().map(|a| a.account_uuid.clone());
    let utilization = crate::claude::statusline::merge(cj.utilization, account.as_deref(), live);
    (
        SeatInfo {
            label: s.label.clone(),
            account: cj.account,
            utilization,
        },
        cj.model_costs,
    )
}
