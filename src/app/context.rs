//! Everything a command needs: dirs, config, seats, prices, a fresh index.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::claude::config_json;
use crate::claude::procs::ProcTable;
use crate::claude::project::Resolver;
use crate::claude::seats::{self, Candidates, Seat, SeatId};
use crate::config::{Config, Dirs};
use crate::core::ids::AccountId;
use crate::core::limits::{Reading, WindowKind};
use crate::core::period::Span;
use crate::core::pricing::PriceBook;
use crate::errors::LookupError;
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
    /// What could not be read and so may make numbers wrong (a damaged `.claude.json`, an
    /// unreadable vault): `doctor` lists them, the dashboard shows them in ATTENTION.
    pub problems: Vec<String>,
}

impl Ctx {
    pub fn load(painter: Painter, mask: bool) -> anyhow::Result<Ctx> {
        let dirs = Dirs::resolve()?;
        let config = Config::load(&dirs)?;
        // SQLite's temporary files (a VACUUM's copy) next to the index, not in a RAM-backed /tmp.
        crate::index::use_temp_dir(&dirs.data.join("tmp"))?;
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
        let mut seat_info = Vec::new();
        let mut caches = Vec::new();
        let mut problems = Vec::new();
        for s in &seats {
            let (info, costs) = seat_info_of(s, &dirs.data, &mut problems);
            if let Some(costs) = costs {
                caches.push((s.id.clone(), costs));
            }
            seat_info.push(info);
        }
        let prices = price_book(&caches, &config, Utc::now())?;
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
            problems,
        };
        // `[storage] keep_raw_days` must fit the seats' transcript retention.
        ctx.keep_raw_days(None)?;
        ctx.check_unconfirmed();
        Ok(ctx)
    }

    /// Days Claude Code keeps each seat's transcripts (`cleanupPeriodDays`, else its default),
    /// and whether the seat sets it.
    pub fn retention(&self) -> Vec<(String, u32, bool)> {
        self.seats
            .iter()
            .map(|s| match s.cleanup_period_days() {
                Some(d) => (s.label.clone(), d, true),
                None => (
                    s.label.clone(),
                    crate::core::storage::DEFAULT_RETENTION_DAYS,
                    false,
                ),
            })
            .collect()
    }

    /// The raw window: `explicit` (`compact --keep-days`), else `[storage] keep_raw_days`, else
    /// the default, checked against the weekly window and the seats' transcript retention
    /// (`core::storage::keep_raw_days`).
    pub fn keep_raw_days(&self, explicit: Option<i64>) -> anyhow::Result<i64> {
        let retention: Vec<(String, u32)> = self
            .retention()
            .into_iter()
            .map(|(s, d, _)| (s, d))
            .collect();
        crate::core::storage::keep_raw_days(
            explicit.or(self.config.storage.keep_raw_days),
            &retention,
        )
        .map_err(|e| {
            let from = if explicit.is_some() {
                "--keep-days"
            } else {
                "[storage] in config.toml"
            };
            anyhow::anyhow!("{from}: {e}")
        })
    }

    /// Re-reads what every seat says about itself (account, limit readings, Claude Code's cached
    /// prices) and the `[[price]]` overrides of `config.toml`. The price book is replaced when its
    /// fingerprint changed, so the next refresh reprices the index with the same book a CLI run
    /// would build now. Returns whether the prices changed; an invalid config is an error and
    /// keeps the prices in force.
    pub fn reload_seat_info(&mut self) -> anyhow::Result<bool> {
        let mut caches = Vec::new();
        let mut problems = Vec::new();
        self.seat_info = self
            .seats
            .iter()
            .map(|s| {
                let (info, costs) = seat_info_of(s, &self.dirs.data, &mut problems);
                if let Some(costs) = costs {
                    caches.push((s.id.clone(), costs));
                }
                info
            })
            .collect();
        self.problems = problems;
        self.check_unconfirmed();
        self.config.price = Config::load(&self.dirs)?.price;
        let prices = price_book(&caches, &self.config, self.now)?;
        if prices.fingerprint() == self.prices.fingerprint() {
            return Ok(false);
        }
        self.prices = prices;
        Ok(true)
    }

    /// [`Ctx::drop_unconfirmed`], with a failure kept in [`Ctx::problems`] (and on the debug
    /// channel) instead of stopping the command: the numbers are shown, with the reason they may
    /// be off.
    pub fn check_unconfirmed(&mut self) {
        if let Err(e) = self.drop_unconfirmed() {
            let msg = format!("vault unreadable, new logins cannot be confirmed: {e:#}");
            crate::diag::log("vault", &msg);
            self.problems.push(msg);
        }
    }

    /// Seats whose current login is not confirmed lose the stale profile, so they are not merged
    /// into another account and do not borrow its limit reading.
    pub fn drop_unconfirmed(&mut self) -> anyhow::Result<()> {
        let mut v = crate::vault::Vault::open(&self.dirs.vault_dir())?;
        // Names and plan tiers of saved logins, for accounts that are in no seat right now.
        self.login_labels.clear();
        self.login_tiers.clear();
        for l in v.logins() {
            if let Some(a) = &l.account_uuid {
                // `--mask` hides a label made from the login's email (`ui::mask`).
                self.login_labels.entry(a.clone()).or_insert_with(|| {
                    crate::ui::mask::login_label(&l.label, l.email.as_deref(), self.mask)
                });
                if let Some(t) = l.plan.clone().or_else(|| l.meta.rate_limit_tier.clone()) {
                    self.login_tiers.insert(a.clone(), t);
                }
            }
        }
        let states = v.edit(|tx| tx.seat_states(&self.seats))?;
        for si in &mut self.seat_info {
            if states
                .get(&si.seat)
                .is_some_and(|st| st.unverified_since.is_some())
            {
                si.account = None;
                si.utilization = None;
            }
        }
        Ok(())
    }

    /// A context over the demo world only: no discovery, its own index and vault.
    pub fn demo(painter: Painter, seats: Vec<Seat>, root: &std::path::Path) -> anyhow::Result<Ctx> {
        let base = Dirs::resolve()?;
        let dirs = Dirs {
            data: root.join("swarmy-data"),
            config: root.join("swarmy-config"),
            home: base.home,
        };
        crate::index::use_temp_dir(&dirs.data.join("tmp"))?;
        let mut seat_info = Vec::new();
        let mut caches = Vec::new();
        for s in &seats {
            let cj = config_json::read_seat_file(&s.claude_json)?;
            if let Some(costs) = cj.model_costs {
                caches.push((s.id.clone(), costs));
            }
            seat_info.push(SeatInfo {
                seat: s.id.clone(),
                label: s.label.clone(),
                account: cj.account,
                utilization: cj.utilization,
            });
        }
        let config = Config::default();
        let prices = price_book(&caches, &config, Utc::now())?;
        let mut ctx = Ctx {
            dirs,
            config,
            seats,
            seat_info,
            prices,
            procs: ProcTable::snapshot(),
            painter,
            mask: false,
            now: Utc::now(),
            login_labels: Default::default(),
            login_tiers: Default::default(),
            problems: Vec::new(),
        };
        ctx.check_unconfirmed();
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
        format!("acct-{}", crate::core::ids::short(uuid))
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

    /// The freshest utilization reading of every account Claude Code has cached in a seat now
    /// (the status line's when newer).
    fn seat_readings(&self) -> Vec<(String, Vec<Reading>)> {
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

    /// The latest reading of every account and window kind: the newest stored in the index, or
    /// the one a seat holds now when newer. Every account ever read is there, not only those in a
    /// seat now: a login parked in the vault keeps its last known limit and reset time. Every
    /// surface (`swarmy limits`, `swarmy accounts`, `doctor`, the dashboard, the status line
    /// through `fleet`) starts from these readings, so they agree on an account.
    pub fn latest_readings(&self, idx: &Index) -> anyhow::Result<Vec<(String, Vec<Reading>)>> {
        let mut merged: std::collections::BTreeMap<String, Vec<Reading>> =
            idx.latest_readings()?.into_iter().collect();
        for (acct, rs) in self.seat_readings() {
            let e = merged.entry(acct).or_default();
            for r in rs {
                match e.iter_mut().find(|x| x.kind == r.kind) {
                    Some(x) if x.fetched_at < r.fetched_at => *x = r,
                    Some(_) => {}
                    None => e.push(r),
                }
            }
        }
        Ok(merged.into_iter().collect())
    }

    /// The weekly window in force (`core::limits::window_of`: a reset by hand moves its start) of
    /// the given account, else of the most recently read one, for `--period week`: the same
    /// window `swarmy limits` and the dashboard show.
    pub fn limit_week(
        &self,
        idx: &Index,
        account: Option<&AccountId>,
    ) -> anyhow::Result<Option<Span>> {
        let readings = self.latest_readings(idx)?;
        let pick = match account {
            Some(a) => readings.iter().find(|(u, _)| u == a.as_str()),
            None => readings
                .iter()
                .max_by_key(|(_, r)| r.iter().map(|x| x.fetched_at).max()),
        };
        let Some((acct, r)) = pick.and_then(|(acct, rs)| {
            rs.iter()
                .find(|r| r.kind == WindowKind::SevenDay)
                .map(|r| (acct, r))
        }) else {
            return Ok(None);
        };
        let history = idx.readings(acct, WindowKind::SevenDay)?;
        let w = crate::core::limits::window_of(r, &history, self.now).window;
        Ok(Some(Span {
            start: w.start,
            end: w.end.min(self.now + chrono::Duration::seconds(1)),
        }))
    }

    /// The account a user named (`--account`): an account uuid, an email, a saved login's label
    /// or a name from `[accounts]` of config.toml, resolved once so every period and filter
    /// treats it the same (review H7). No match, or several accounts behind one name, is an
    /// error that lists what is known.
    pub fn resolve_account(&self, idx: &Index, input: &str) -> anyhow::Result<AccountId> {
        let key = input.trim();
        let mut found: std::collections::BTreeSet<AccountId> =
            idx.accounts_matching(key)?.into_iter().collect();
        for a in self.seat_info.iter().filter_map(|s| s.account.as_ref()) {
            let email = a
                .email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(key));
            if a.account_uuid == key || email {
                found.insert(AccountId::new(a.account_uuid.clone()));
            }
        }
        for (uuid, name) in &self.config.accounts {
            if name == key || uuid == key {
                found.insert(AccountId::new(uuid.clone()));
            }
        }
        // Saved logins: their labels name accounts too.
        match crate::vault::Vault::open(&self.dirs.vault_dir()) {
            Ok(v) => {
                for l in v.logins() {
                    let email = l
                        .email
                        .as_deref()
                        .is_some_and(|e| e.eq_ignore_ascii_case(key));
                    if let Some(a) = &l.account_uuid
                        && (l.label == key || email || a == key)
                    {
                        found.insert(AccountId::new(a.clone()));
                    }
                }
            }
            Err(e) => crate::diag::log(
                "accounts",
                &format!("cannot read the vault, login labels are not matched: {e:#}"),
            ),
        }
        let name = |a: &AccountId| format!("{} ({a})", self.account_name(a.as_str()));
        let mut it = found.iter();
        match (it.next(), it.next()) {
            (Some(one), None) => Ok(one.clone()),
            (None, _) => Err(LookupError::NoAccount(key.to_string()).into()),
            (Some(_), Some(_)) => Err(LookupError::AmbiguousAccount {
                key: key.to_string(),
                candidates: found.iter().map(name).collect(),
            }
            .into()),
        }
    }

    /// The seat a user named: by its label, else by its id. An unknown name is an error that
    /// lists the seats.
    pub fn resolve_seat(&self, name: &str) -> anyhow::Result<&Seat> {
        self.seats
            .iter()
            .find(|s| s.label == name)
            .or_else(|| self.seats.iter().find(|s| s.id.as_str() == name))
            .ok_or_else(|| {
                LookupError::NoSeat {
                    name: name.to_string(),
                    known: self.seats.iter().map(|s| s.label.clone()).collect(),
                }
                .into()
            })
    }

    /// The label of a seat id: the seat's current label, else the id itself (a seat that is
    /// not discovered in this run).
    pub fn seat_label(&self, id: &SeatId) -> String {
        self.seats
            .iter()
            .find(|s| &s.id == id)
            .map_or_else(|| id.to_string(), |s| s.label.clone())
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

/// The prices in force: the built-in table, Claude Code's cached prices of every seat (in seat
/// order), then the user's `[[price]]` overrides. The CLI and the dashboard build it the same way,
/// so they agree on the fingerprint and never reprice the index back and forth.
pub fn price_book(
    caches: &[(SeatId, serde_json::Value)],
    config: &Config,
    now: DateTime<Utc>,
) -> anyhow::Result<PriceBook> {
    let mut prices = PriceBook::builtin();
    for (seat, costs) in caches {
        prices.merge_claude_code_cache(seat, costs);
    }
    config.apply_prices(&mut prices, now.with_timezone(&chrono::Local).date_naive())?;
    Ok(prices)
}

/// What a seat says about itself: its account from `.claude.json`, and its limit readings — the
/// status line's when newer than Claude Code's cached ones (see `claude::statusline`).
/// A file that cannot be read adds to `problems` (the seat then shows no account and no reading,
/// and says why).
pub fn seat_info_of(
    s: &Seat,
    data_dir: &std::path::Path,
    problems: &mut Vec<String>,
) -> (SeatInfo, Option<serde_json::Value>) {
    let cj = match config_json::read_seat_file(&s.claude_json) {
        Ok(cj) => cj,
        Err(e) => {
            let msg = format!("seat {}: {e:#}", s.label);
            crate::diag::log("seats", &msg);
            problems.push(msg);
            config_json::ClaudeJson::default()
        }
    };
    let live =
        crate::claude::statusline::read(&crate::claude::statusline::file_of(data_dir, &s.id));
    let account = cj.account.as_ref().map(|a| a.account_uuid.clone());
    let utilization = crate::claude::statusline::merge(cj.utilization, account.as_deref(), live);
    (
        SeatInfo {
            seat: s.id.clone(),
            label: s.label.clone(),
            account: cj.account,
            utilization,
        },
        cj.model_costs,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First review H5: a damaged `.claude.json` is a problem that is reported, never a seat
    /// without an account; a seat that has none yet is no problem.
    #[test]
    fn a_damaged_seat_file_is_reported_not_taken_for_empty() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join(".claude-work");
        std::fs::create_dir_all(&dir).unwrap();
        let seat = Seat::new("work", dir.clone(), dir.join(".claude.json"), false);
        let mut problems = Vec::new();
        let (info, _) = seat_info_of(&seat, t.path(), &mut problems);
        assert!(info.account.is_none());
        assert!(problems.is_empty(), "no file yet: {problems:?}");
        std::fs::write(&seat.claude_json, "{\"oauthAccount\": {\"accountUuid\": ").unwrap();
        let (info, _) = seat_info_of(&seat, t.path(), &mut problems);
        assert!(info.account.is_none());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("seat work:") && problems[0].contains("damaged"));
        // The vault refuses to read the seat rather than save a login without its account.
        assert!(crate::vault::read_seat(&seat).is_err());
    }
}
