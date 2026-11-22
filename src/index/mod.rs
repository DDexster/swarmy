//! The local index: an SQLite cache of every transcript, filled incrementally, next to the facts
//! swarmy observed over time (logins per seat, limit readings, price versions; see `schema`).
//!
//! Transcripts are append-only, so each file is read from where the previous run stopped
//! (tracked by inode, size and offset). A file that shrank or was replaced is read again from the
//! start. The first run reads everything in parallel.

pub mod insights;
pub mod live;
mod prices;
pub mod query;
#[cfg(test)]
pub(crate) mod scenarios;
pub mod schema;

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use chrono::{DateTime, TimeZone, Utc};
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};

use crate::claude::config_json::{Account, Utilization};
use crate::claude::files::{self, FileKind, TranscriptFile};
use crate::claude::project::Resolver;
use crate::claude::seats::Seat;
use crate::claude::transcript::{self, Event, TitleKind};
use crate::core::pricing::PriceBook;

/// The account of a request from its seat's login history (see `seat_accounts`).
const SEAT_ACCOUNT_AT: &str = "COALESCE((SELECT sa.account_uuid FROM seat_accounts sa WHERE sa.seat = requests.seat AND sa.ts <= requests.ts ORDER BY sa.ts DESC LIMIT 1), requests.account_uuid)";

/// Revision of the cwd → project rules; a change recomputes the `projects` table.
const PROJECTS_REV: &str = "4";

pub struct Index {
    conn: Connection,
    /// Session dirs listed by the hot refresh, with their modification time: a dir is listed
    /// again only when a file was added to it.
    session_dirs: HashMap<(String, String), ListedDirs>,
    /// The last price book dated by `dated_prices`, by the fingerprint of the undated one.
    dated: Option<(String, PriceBook)>,
}

/// The dirs a session's files were listed from (with their mtimes), and the files.
type ListedDirs = (
    Vec<(std::path::PathBuf, Option<std::time::SystemTime>)>,
    Vec<TranscriptFile>,
);

/// What `.claude.json` of a seat says right now.
#[derive(Debug, Clone, Default)]
pub struct SeatInfo {
    pub label: String,
    pub account: Option<Account>,
    pub utilization: Option<Utilization>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    pub files_done: usize,
    pub files_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Debug, Clone, Default)]
pub struct RefreshStats {
    pub files_total: usize,
    pub files_read: usize,
    pub bytes_read: u64,
    pub requests_new: u64,
    pub elapsed: Duration,
}

struct Work {
    file: TranscriptFile,
    start: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    account: Option<String>,
    last_ts: i64,
    reset: bool,
}

pub fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

pub fn from_ms(t: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(t)
        .single()
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

#[cfg(unix)]
fn inode(m: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.ino()
}

#[cfg(not(unix))]
fn inode(_m: &std::fs::Metadata) -> u64 {
    0
}

impl Index {
    pub fn open(path: &Path) -> anyhow::Result<Index> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> anyhow::Result<Index> {
        conn.busy_timeout(Duration::from_secs(10))?;
        // The live view runs a couple of dozen distinct statements every second (`live.rs`);
        // keep them all prepared instead of the default 16.
        conn.set_prepared_statement_cache_capacity(64);
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Facts migrate, the cache is rebuilt when its version changed; requests read again
        // then follow their seat's login history like any new request (`post_pass`).
        schema::migrate(&conn)?;
        Ok(Index {
            conn,
            session_dirs: HashMap::new(),
            dated: None,
        })
    }

    pub fn meta(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(schema::meta(&self.conn, key)?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        Ok(schema::set_meta(&self.conn, key, value)?)
    }

    /// The database file (`None` in memory): tells two indexes apart in a process-wide memo.
    pub fn path(&self) -> Option<&str> {
        self.conn.path()
    }

    /// Brings the index up to date with every seat's transcripts.
    pub fn refresh(
        &mut self,
        seats: &[Seat],
        seat_info: &[SeatInfo],
        resolver: &mut Resolver,
        prices: &PriceBook,
        progress: &mut dyn FnMut(Progress),
    ) -> anyhow::Result<RefreshStats> {
        let all: Vec<TranscriptFile> = seats.iter().flat_map(files::list).collect();
        self.refresh_files(all, seat_info, resolver, prices, progress)
    }

    /// A cheap refresh for the live view: only files touched in the last 30 minutes and the files
    /// of running sessions (including subagents spawned since the last full scan).
    pub fn refresh_hot(
        &mut self,
        seats: &[Seat],
        live: &[(String, String)],
        seat_info: &[SeatInfo],
        resolver: &mut Resolver,
        prices: &PriceBook,
    ) -> anyhow::Result<RefreshStats> {
        let since = Utc::now().timestamp() - 1800;
        let mut out: Vec<TranscriptFile> = Vec::new();
        {
            let mut st = self.conn.prepare(
                "SELECT path, seat, session_id, agent_id, kind FROM files WHERE mtime >= ?1",
            )?;
            let rows = st.query_map([since], |r| {
                let kind: String = r.get(4)?;
                Ok(TranscriptFile {
                    path: std::path::PathBuf::from(r.get::<_, String>(0)?),
                    seat: r.get(1)?,
                    session_id: r.get(2)?,
                    agent_id: r.get(3)?,
                    kind: match kind.as_str() {
                        "main" => FileKind::Main,
                        "subagent" => FileKind::Subagent,
                        _ => FileKind::Workflow(String::new()),
                    },
                })
            })?;
            for r in rows {
                out.push(r?);
            }
        }
        for (seat_label, sid) in live {
            let Some(seat) = seats.iter().find(|s| &s.label == seat_label) else {
                continue;
            };
            out.extend(self.session_files(seat, sid));
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|f| seen.insert(f.path.clone()));
        let mut quiet = |_p: Progress| {};
        self.refresh_files(out, seat_info, resolver, prices, &mut quiet)
    }

    /// The files of a live session, listed again only when one of its dirs changed.
    fn session_files(&mut self, seat: &Seat, sid: &str) -> Vec<TranscriptFile> {
        let key = (seat.label.clone(), sid.to_string());
        let mtime = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        if let Some((dirs, cached)) = self.session_dirs.get(&key)
            && !dirs.is_empty()
            && dirs.iter().all(|(d, t)| mtime(d) == *t)
        {
            return cached.clone();
        }
        let list = files::list_session(seat, sid);
        // Watch the dirs that hold these files (main dir, subagents, workflow runs).
        let mut dirs: Vec<std::path::PathBuf> = list
            .iter()
            .filter_map(|f| f.path.parent().map(|p| p.to_path_buf()))
            .collect();
        // Also dirs that may not exist yet: creating them is how a first subagent shows up.
        for f in list.iter().filter(|f| f.kind == FileKind::Main) {
            if let Some(pdir) = f.path.parent() {
                let base = pdir.join(sid);
                dirs.push(base.join("subagents").join("workflows"));
                dirs.push(base.join("subagents"));
                dirs.push(base);
            }
        }
        dirs.sort();
        dirs.dedup();
        let stamped = dirs
            .into_iter()
            .map(|d| {
                let t = mtime(&d);
                (d, t)
            })
            .collect();
        self.session_dirs.insert(key, (stamped, list.clone()));
        list
    }

    fn refresh_files(
        &mut self,
        all: Vec<TranscriptFile>,
        seat_info: &[SeatInfo],
        resolver: &mut Resolver,
        prices: &PriceBook,
        progress: &mut dyn FnMut(Progress),
    ) -> anyhow::Result<RefreshStats> {
        let started = Instant::now();
        let mut known: HashMap<String, (u64, u64, u64, Option<String>, i64)> = HashMap::new();
        {
            let mut st = self
                .conn
                .prepare("SELECT path, ino, size, offset, account_uuid, last_ts FROM files")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (
                        r.get::<_, i64>(1)? as u64,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, i64>(3)? as u64,
                        r.get(4)?,
                        r.get(5)?,
                    ),
                ))
            })?;
            for row in rows {
                let (k, v) = row?;
                known.insert(k, v);
            }
        }

        let mut work = Vec::new();
        for f in all.iter() {
            let Ok(m) = std::fs::metadata(&f.path) else {
                continue;
            };
            let (ino, size) = (inode(&m), m.len());
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let key = f.path.to_string_lossy().into_owned();
            let (start, account, last_ts, reset) = match known.get(&key) {
                Some((kino, ksize, ..)) if *kino == ino && *ksize == size => continue,
                Some((kino, _, koff, kacc, kts)) if *kino == ino && size >= *koff => {
                    (*koff, kacc.clone(), *kts, false)
                }
                Some(_) => (0, None, 0, true),
                None => (0, None, 0, false),
            };
            work.push(Work {
                file: f.clone(),
                start,
                ino,
                size,
                mtime,
                account,
                last_ts,
                reset,
            });
        }

        let bytes_total: u64 = work.iter().map(|w| w.size - w.start).sum();
        let files_total = work.len();
        let mut stats = RefreshStats {
            files_total: all.len(),
            files_read: files_total,
            ..Default::default()
        };
        let mut done = Progress {
            files_done: 0,
            files_total,
            bytes_done: 0,
            bytes_total,
        };
        progress(done);

        // Big files first so the parallel batches stay balanced.
        work.sort_by_key(|w| std::cmp::Reverse(w.size - w.start));
        let before: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM requests", [], |r| r.get(0))?;
        // Rows added by this refresh have a larger rowid (requests are never deleted).
        let last_rowid: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(rowid), 0) FROM requests", [], |r| {
                    r.get(0)
                })?;
        let mut type_counts: HashMap<String, u64> = HashMap::new();
        let mut cwds: std::collections::HashSet<String> = std::collections::HashSet::new();
        for batch in work.chunks(48) {
            // A few small files parse faster on this thread than by waking the whole pool.
            let parsed: Vec<(&Work, std::io::Result<transcript::Chunk>)> = if batch.len() < 8 {
                batch
                    .iter()
                    .map(|w| (w, transcript::read_from(&w.file.path, w.start)))
                    .collect()
            } else {
                batch
                    .par_iter()
                    .map(|w| (w, transcript::read_from(&w.file.path, w.start)))
                    .collect()
            };
            let tx = self.conn.transaction()?;
            for (w, chunk) in parsed {
                let Ok(chunk) = chunk else { continue };
                done.bytes_done += chunk.end - w.start;
                for (t, n) in &chunk.stats.by_type {
                    *type_counts.entry(t.clone()).or_default() += n;
                }
                write_file(&tx, w, chunk, &mut cwds)?;
            }
            tx.commit()?;
            done.files_done += batch.len();
            progress(done);
        }
        stats.bytes_read = done.bytes_done;

        self.post_pass(
            seat_info,
            resolver,
            prices,
            &type_counts,
            &cwds,
            (files_total > 0).then_some(last_rowid),
        )?;
        let after: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM requests", [], |r| r.get(0))?;
        stats.requests_new = (after - before).max(0) as u64;
        stats.elapsed = started.elapsed();
        self.set_meta("refreshed_at", &Utc::now().timestamp_millis().to_string())?;
        Ok(stats)
    }

    fn post_pass(
        &mut self,
        seat_info: &[SeatInfo],
        resolver: &mut Resolver,
        prices: &PriceBook,
        types: &HashMap<String, u64>,
        new_cwds: &std::collections::HashSet<String>,
        new_after_rowid: Option<i64>,
    ) -> anyhow::Result<()> {
        let now = ms(Utc::now());
        let tx = self.conn.transaction()?;
        for si in seat_info {
            if let Some(a) = &si.account {
                tx.execute(
                    "INSERT INTO accounts(account_uuid, email, display_name, org_name, plan, tier, seat, seen_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(account_uuid) DO UPDATE SET email = excluded.email, display_name = excluded.display_name,
                       org_name = excluded.org_name, plan = excluded.plan, tier = excluded.tier, seat = excluded.seat, seen_at = excluded.seen_at",
                    params![a.account_uuid, a.email, a.display_name, a.organization_name, a.plan, a.rate_limit_tier, si.label, now],
                )?;
            }
            if let Some(u) = &si.utilization {
                for r in &u.readings {
                    tx.execute(
                        "INSERT INTO readings(account_uuid, kind, fetched_at, utilization, resets_at) VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(account_uuid, kind, fetched_at) DO NOTHING",
                        params![u.account_uuid, r.kind.key(), ms(r.fetched_at), r.utilization, ms(r.resets_at)],
                    )?;
                }
            }
        }
        // What each seat has logged in now, with the time the seat itself recorded it. A change of
        // account re-attributes the seat's requests from that moment on.
        let mut reattribute: Vec<(String, i64)> = Vec::new();
        for si in seat_info {
            let Some(a) = &si.account else { continue };
            let mut events: Vec<(i64, &str)> = Vec::new();
            if let Some(t) = a.profile_fetched_at {
                events.push((ms(t), a.account_uuid.as_str()));
            }
            if let Some(u) = si
                .utilization
                .as_ref()
                .filter(|u| u.account_uuid == a.account_uuid)
            {
                events.push((ms(u.fetched_at), u.account_uuid.as_str()));
            }
            for (ts, acc) in events {
                let before: Option<String> = tx
                    .query_row(
                        "SELECT account_uuid FROM seat_accounts WHERE seat = ?1 AND ts <= ?2 ORDER BY ts DESC LIMIT 1",
                        params![si.label, ts],
                        |r| r.get(0),
                    )
                    .optional()?;
                // Only changes are kept: readings arrive after every response.
                if before.as_deref() == Some(acc) {
                    continue;
                }
                let added = tx.execute(
                    "INSERT INTO seat_accounts(seat, ts, account_uuid) VALUES (?1, ?2, ?3)
                     ON CONFLICT(seat, ts) DO NOTHING",
                    params![si.label, ts, acc],
                )?;
                if added > 0 {
                    reattribute.push((si.label.clone(), ts));
                }
            }
        }
        for (seat, ts) in &reattribute {
            tx.execute(
                &format!("UPDATE requests SET account_uuid = {SEAT_ACCOUNT_AT} WHERE seat = ?1 AND ts >= ?2"),
                params![seat, ts],
            )?;
        }
        let rev_ok = schema::meta(&tx, "projects_rev")?.as_deref() == Some(PROJECTS_REV);
        let Some(new_after_rowid) = new_after_rowid.or((!rev_ok).then_some(i64::MAX)) else {
            tx.commit()?;
            return self.price_all(prices);
        };
        // New requests follow their seat's login history too: requests read for the first time,
        // which after a cache rebuild is all of them.
        tx.execute(
            &format!(
                "UPDATE requests SET account_uuid = {SEAT_ACCOUNT_AT}
                 WHERE rowid > ?1 AND EXISTS (SELECT 1 FROM seat_accounts sa WHERE sa.seat = requests.seat AND sa.ts <= requests.ts)"
            ),
            params![new_after_rowid],
        )?;
        // The account of subagent requests comes from the session's owner at that time.
        tx.execute_batch(
            "UPDATE requests SET account_uuid = (SELECT o.account_uuid FROM owners o WHERE o.seat = requests.seat
               AND o.session_id = requests.session_id AND o.ts <= requests.ts ORDER BY o.ts DESC LIMIT 1)
             WHERE account_uuid IS NULL;
             UPDATE requests SET account_uuid = (SELECT o.account_uuid FROM owners o WHERE o.seat = requests.seat
               AND o.session_id = requests.session_id ORDER BY o.ts ASC LIMIT 1)
             WHERE account_uuid IS NULL;",
        )?;
        // Transcripts written before Claude Code recorded owners: the seat's current account.
        for si in seat_info {
            if let Some(a) = &si.account {
                tx.execute("UPDATE requests SET account_uuid = ?1 WHERE account_uuid IS NULL AND seat = ?2", params![a.account_uuid, si.label])?;
            }
        }
        // Projects of new working directories. Rule revision 2 (sibling worktrees, orphans taking
        // the session's project) recomputes every cwd once.
        let cwds: Vec<String> = if rev_ok {
            let mut known_cwd = tx.prepare("SELECT 1 FROM projects WHERE cwd = ?1")?;
            new_cwds
                .iter()
                .filter(|c| !known_cwd.exists([c.as_str()]).unwrap_or(false))
                .cloned()
                .collect()
        } else {
            tx.execute("DELETE FROM projects", [])?;
            let mut st = tx.prepare("SELECT DISTINCT cwd FROM requests WHERE cwd IS NOT NULL")?;

            st.query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut orphans: Vec<String> = Vec::new();
        for cwd in &cwds {
            let p = resolver.resolve(cwd);
            if p.orphan {
                orphans.push(cwd.clone());
            }
            tx.execute(
                "INSERT OR REPLACE INTO projects(cwd, key, name, worktree) VALUES (?1, ?2, ?3, ?4)",
                params![cwd, p.key, p.name, p.worktree],
            )?;
        }
        if !orphans.is_empty() {
            // A dir outside any repository (a removed worktree, a scratch dir) used by a session
            // that runs in a repository belongs to that repository's project.
            let set: std::collections::HashSet<&str> = orphans.iter().map(String::as_str).collect();
            let mut votes: HashMap<String, HashMap<String, i64>> = HashMap::new();
            {
                let mut st = tx.prepare(
                    "SELECT r.cwd, s.cwd, count(*) FROM requests r JOIN sessions s ON s.seat = r.seat AND s.session_id = r.session_id
                     WHERE s.cwd IS NOT NULL AND s.cwd != r.cwd GROUP BY r.cwd, s.cwd",
                )?;
                let rows = st.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?;
                for row in rows {
                    let (cwd, main, n) = row?;
                    if set.contains(cwd.as_str()) {
                        *votes.entry(cwd).or_default().entry(main).or_default() += n;
                    }
                }
            }
            // Never fold into a home dir or anything as shallow (`/home/x/work` is the least).
            let under = |dir: &str, root: &str| {
                root.matches(['/', '\\']).count() >= 4
                    && dir.starts_with(root)
                    && dir[root.len()..].starts_with(['/', '\\'])
            };
            let mut done: std::collections::HashSet<String> = Default::default();
            for (cwd, mains) in votes {
                // A repository the session runs in; else the session's own dir when this one lies
                // under it (subdirs and `.claude/worktrees/*` of a dir that is gone now).
                let best = mains
                    .iter()
                    .map(|(m, n)| (resolver.resolve(m), *n))
                    .filter(|(p, _)| !p.orphan)
                    .max_by_key(|(_, n)| *n)
                    .map(|x| x.0)
                    .or_else(|| {
                        mains
                            .keys()
                            .filter(|m| under(&cwd, m))
                            .max_by_key(|m| m.len())
                            .map(|m| resolver.resolve(m))
                    });
                if let Some(p) = best {
                    tx.execute(
                        "UPDATE projects SET key = ?2, name = ?3, worktree = ?1 WHERE cwd = ?1",
                        params![cwd, p.key, p.name],
                    )?;
                    done.insert(cwd);
                }
            }
        }
        if !rev_ok {
            schema::set_meta(&tx, "projects_rev", PROJECTS_REV)?;
        }
        for (t, n) in types {
            tx.execute("INSERT INTO record_types(type, count) VALUES (?1, ?2) ON CONFLICT(type) DO UPDATE SET count = count + excluded.count", params![t, *n as i64])?;
        }
        tx.commit()?;
        self.price_all(prices)?;
        Ok(())
    }
}

#[derive(Default)]
struct FileAgg {
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    cwd: Option<String>,
    title: Option<String>,
    ai_title: Option<String>,
    compactions: i64,
}

fn write_file(
    tx: &rusqlite::Transaction,
    w: &Work,
    chunk: transcript::Chunk,
    cwds: &mut std::collections::HashSet<String>,
) -> anyhow::Result<()> {
    let f = &w.file;
    let main = f.kind == FileKind::Main;
    if w.reset {
        // The file is read again from the start: forget what it added, so nothing counts twice.
        // (Request rows merge by `MAX`; tool counts and compactions are sums.)
        tx.execute(
            "UPDATE requests SET tools = 0 WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3",
            params![f.seat, f.session_id, f.agent_id],
        )?;
        if main {
            tx.execute(
                "DELETE FROM prompts WHERE seat = ?1 AND session_id = ?2",
                params![f.seat, f.session_id],
            )?;
            tx.execute(
                "UPDATE sessions SET compactions = 0 WHERE seat = ?1 AND session_id = ?2",
                params![f.seat, f.session_id],
            )?;
        }
        tx.execute(
            "DELETE FROM api_errors WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3",
            params![f.seat, f.session_id, f.agent_id],
        )?;
    }
    let mut account = w.account.clone();
    let mut last_ts = w.last_ts;
    let mut agg = FileAgg::default();
    let mut ins_req = tx.prepare_cached(
        "INSERT INTO requests(msg_id, seat, session_id, agent_id, account_uuid, ts, model, requested_model, effort, fast, cwd,
                              input, cw5, cw1h, cread, output, thinking, web, tools)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(msg_id) DO UPDATE SET
           input = MAX(input, excluded.input), cw5 = MAX(cw5, excluded.cw5), cw1h = MAX(cw1h, excluded.cw1h),
           cread = MAX(cread, excluded.cread), output = MAX(output, excluded.output), thinking = MAX(thinking, excluded.thinking),
           web = MAX(web, excluded.web), tools = tools + excluded.tools,
           account_uuid = COALESCE(account_uuid, excluded.account_uuid), cost = NULL, priced = NULL",
    )?;
    let mut ins_tool = tx.prepare_cached(
        "INSERT INTO tool_calls(id, msg_id, seat, session_id, agent_id, ts, name, summary, target) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO NOTHING",
    )?;
    let mut upd_tool =
        tx.prepare_cached("UPDATE tool_calls SET done_ts = ?1, is_error = ?2 WHERE id = ?3")?;
    for ev in chunk.events {
        match ev {
            Event::Owner { account_uuid } => {
                tx.execute("INSERT INTO owners(seat, session_id, ts, account_uuid) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING", params![f.seat, f.session_id, last_ts, account_uuid])?;
                account = Some(account_uuid);
            }
            Event::Request(r) => {
                let t = ms(r.ts);
                last_ts = last_ts.max(t);
                agg.first_ts = Some(agg.first_ts.map_or(t, |x| x.min(t)));
                agg.last_ts = Some(agg.last_ts.map_or(t, |x| x.max(t)));
                if r.cwd.is_some() {
                    agg.cwd = r.cwd.clone();
                    if let Some(c) = &r.cwd
                        && !cwds.contains(c)
                    {
                        cwds.insert(c.clone());
                    }
                }
                let u = r.usage;
                ins_req.execute(params![
                    r.msg_id,
                    f.seat,
                    f.session_id,
                    f.agent_id,
                    if main { account.clone() } else { None },
                    t,
                    r.model,
                    r.requested_model,
                    r.effort,
                    r.fast as i64,
                    r.cwd,
                    u.input as i64,
                    u.cache_write_5m as i64,
                    u.cache_write_1h as i64,
                    u.cache_read as i64,
                    u.output as i64,
                    u.thinking as i64,
                    u.web_search as i64,
                    r.tool_uses.len() as i64,
                ])?;
                for tu in &r.tool_uses {
                    if !tu.id.is_empty() {
                        ins_tool.execute(params![
                            tu.id,
                            r.msg_id,
                            f.seat,
                            f.session_id,
                            f.agent_id,
                            t,
                            tu.name,
                            tu.summary,
                            tu.target
                        ])?;
                    }
                }
            }
            Event::ToolResult {
                tool_use_id,
                ts,
                is_error,
            } => {
                if let Some(t) = ts {
                    last_ts = last_ts.max(ms(t));
                }
                upd_tool.execute(params![ts.map(ms), is_error as i64, tool_use_id])?;
            }
            Event::Prompt { ts, text } => {
                if let Some(t) = ts {
                    last_ts = last_ts.max(ms(t));
                    if main {
                        tx.execute("INSERT INTO prompts(seat, session_id, ts, chars) VALUES (?1, ?2, ?3, ?4)", params![f.seat, f.session_id, ms(t), text.chars().count() as i64])?;
                    }
                }
            }
            Event::Title { kind, text } => match kind {
                TitleKind::Custom => agg.title = Some(text),
                TitleKind::Ai => agg.ai_title = Some(text),
                TitleKind::AgentName => {
                    agg.title.get_or_insert(text);
                }
            },
            Event::TurnEnd { ts, .. } => {
                if let Some(t) = ts {
                    last_ts = last_ts.max(ms(t));
                }
            }
            Event::Compact { .. } => agg.compactions += 1,
            Event::AgentLaunch { .. } => {}
            Event::CostState { total_usd, models } => {
                // Cumulative per process; a resumed session starts a new count, so keep the largest.
                tx.execute(
                    "INSERT INTO cost_states(seat, session_id, total_usd, models) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(seat, session_id) DO UPDATE SET total_usd = excluded.total_usd, models = excluded.models
                     WHERE excluded.total_usd >= cost_states.total_usd",
                    params![f.seat, f.session_id, total_usd, serde_json::to_string(&models).unwrap_or_default()],
                )?;
            }
            Event::ApiError { ts, kind, text } => {
                let t = ts.map(ms).unwrap_or(last_ts);
                tx.execute(
                    "INSERT INTO api_errors(seat, session_id, agent_id, ts, kind, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![f.seat, f.session_id, f.agent_id, t, kind, text],
                )?;
            }
        }
    }

    if main {
        if let Some(dir) = f.path.parent().map(|p| p.join(&f.session_id))
            && let Some(t) = files::read_custom_title(&dir)
        {
            agg.title = Some(t);
        }
        tx.execute(
            "INSERT INTO sessions(seat, session_id, title, ai_title, cwd, first_ts, last_ts, compactions) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(seat, session_id) DO UPDATE SET
               title = COALESCE(excluded.title, title), ai_title = COALESCE(excluded.ai_title, ai_title), cwd = COALESCE(excluded.cwd, cwd),
               first_ts = MIN(COALESCE(first_ts, excluded.first_ts), COALESCE(excluded.first_ts, first_ts)),
               last_ts = MAX(COALESCE(last_ts, excluded.last_ts), COALESCE(excluded.last_ts, last_ts)),
               compactions = compactions + excluded.compactions",
            params![f.seat, f.session_id, agg.title, agg.ai_title, agg.cwd, agg.first_ts, agg.last_ts, agg.compactions],
        )?;
    }
    let meta = f
        .meta_path()
        .and_then(|p| files::read_meta(&p))
        .unwrap_or_default();
    let workflow_run = match &f.kind {
        FileKind::Workflow(r) => Some(r.clone()),
        _ => None,
    };
    tx.execute(
        "INSERT INTO agents(seat, session_id, agent_id, kind, workflow_run, agent_type, description, tool_use_id, parent_agent_id, depth,
                            requested_model, effort, finished, first_ts, last_ts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(seat, session_id, agent_id) DO UPDATE SET
           agent_type = COALESCE(excluded.agent_type, agent_type), description = COALESCE(excluded.description, description),
           tool_use_id = COALESCE(excluded.tool_use_id, tool_use_id), parent_agent_id = COALESCE(excluded.parent_agent_id, parent_agent_id),
           depth = COALESCE(excluded.depth, depth), requested_model = COALESCE(excluded.requested_model, requested_model),
           effort = COALESCE(excluded.effort, effort), finished = excluded.finished,
           first_ts = MIN(COALESCE(first_ts, excluded.first_ts), COALESCE(excluded.first_ts, first_ts)),
           last_ts = MAX(COALESCE(last_ts, excluded.last_ts), COALESCE(excluded.last_ts, last_ts))",
        params![
            f.seat,
            f.session_id,
            f.agent_id,
            f.kind.tag(),
            workflow_run,
            meta.agent_type,
            meta.description,
            meta.tool_use_id,
            meta.parent_agent_id,
            meta.spawn_depth.map(|d| d as i64).or(if main { Some(0) } else { None }),
            meta.model,
            meta.effort,
            (meta.finished || main) as i64,
            agg.first_ts,
            agg.last_ts,
        ],
    )?;
    tx.execute(
        "INSERT INTO files(path, seat, session_id, agent_id, kind, ino, size, mtime, offset, last_ts, account_uuid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(path) DO UPDATE SET ino = excluded.ino, size = excluded.size, mtime = excluded.mtime, offset = excluded.offset,
           last_ts = excluded.last_ts, account_uuid = excluded.account_uuid",
        params![
            f.path.to_string_lossy(),
            f.seat,
            f.session_id,
            f.agent_id,
            f.kind.tag(),
            w.ino as i64,
            w.size as i64,
            w.mtime,
            chunk.end as i64,
            last_ts,
            account,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_index_opens_and_reopens() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("i.sqlite");
        let i = Index::open(&p).unwrap();
        assert_eq!(i.count("requests").unwrap(), 0);
        drop(i);
        assert!(Index::open(&p).is_ok());
    }
}
