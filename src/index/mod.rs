//! The local index: an SQLite cache of every transcript, filled incrementally, next to the facts
//! swarmy observed over time (logins per seat, limit readings, price versions; see `schema`).
//!
//! Transcripts are append-only, so each file is read from where the previous run stopped
//! (tracked by inode, size and offset). A file that shrank or was replaced is read again from the
//! start. The first run reads everything in parallel.

pub mod archive;
#[cfg(test)]
pub(crate) mod archive_scenarios;
pub mod compact;
#[cfg(test)]
mod compact_scenarios;
pub mod insights;
pub mod live;
mod marks;
mod prices;
mod projects;
pub use prices::BandDrift;
pub mod query;
#[cfg(test)]
pub(crate) mod scenarios;
pub mod schema;
pub mod storage;
mod write;

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use chrono::{DateTime, TimeZone, Utc};
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::claude::config_json::{Account, Utilization};
use crate::claude::files::{self, FileKind, TranscriptFile};
use crate::claude::project::Resolver;
use crate::claude::seats::Seat;
use crate::claude::transcript;
use crate::core::pricing::PriceBook;

/// The account of a request from its seat's login history (see `seat_accounts`).
const SEAT_ACCOUNT_AT: &str = "COALESCE((SELECT sa.account_uuid FROM seat_accounts sa WHERE sa.seat = requests.seat AND sa.ts <= requests.ts ORDER BY sa.ts DESC LIMIT 1), requests.account_uuid)";

/// Revision of the cwd → project rules; a change recomputes the `projects` table.
const PROJECTS_REV: &str = "5";

pub struct Index {
    conn: Connection,
    /// Session dirs listed by the hot refresh, with their modification time: a dir is listed
    /// again only when a file was added to it.
    session_dirs: HashMap<(String, String), ListedDirs>,
    /// The last price book dated by `dated_prices`, by the fingerprint of the undated one.
    dated: Option<(String, PriceBook)>,
    /// Settled spend already summed (`query::settled_total`).
    settled: std::cell::RefCell<HashMap<SettledKey, f64>>,
}

/// A settled total: (settled generation, price fingerprint, account, span start, span end).
type SettledKey = (Option<String>, String, String, i64, i64);

/// How long a write waits for another process's write (a compaction chunk, a VACUUM).
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// The WAL file is cut back to this many bytes after a checkpoint.
const JOURNAL_SIZE_LIMIT: i64 = 32 << 20;

/// Points SQLite's temporary files (the copy a `VACUUM` writes, large sorts) at `dir`, inside
/// swarmy's data dir, instead of the system's temp dir, which is often in RAM. Process-wide, set
/// once (the first call wins).
pub fn use_temp_dir(dir: &Path) -> anyhow::Result<()> {
    static SET: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if SET.get().is_some() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Connection::open_in_memory()?.pragma_update(
        None,
        "temp_store_directory",
        dir.to_string_lossy().as_ref(),
    )?;
    let _first = SET.set(());
    Ok(())
}

/// Spend older than this no longer changes on its own: requests of the last minutes are still
/// being written. Older spend changes only when the index says so, by bumping the settled
/// generation (`bump_settled`): a reprice, a re-attribution to another account, a request indexed
/// late (a file found by the full scan, a file read again).
pub const SETTLED_AFTER_MS: i64 = 5 * 60 * 1000;

/// A reading equal to a stored one (account, window, value) at most this far away is not stored.
const READING_REPEAT_MS: i64 = 5 * 60 * 1000;

/// Marks a change to settled spend (see [`SETTLED_AFTER_MS`]), inside the writing transaction.
fn bump_settled(tx: &Connection) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO meta(key, value) VALUES ('settled_gen', '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [],
    )?;
    Ok(())
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
    /// Records read again that were rolled up already, and skipped (`marks`).
    pub records_skipped: u64,
    pub elapsed: Duration,
}

struct Work {
    file: TranscriptFile,
    /// The file's `(ino, offset)` in the index when the work was planned (`None`: not indexed).
    /// It is checked again inside the write transaction: when another process advanced the file
    /// meanwhile, this work is dropped instead of adding the same bytes twice.
    expected: Option<(u64, u64)>,
    start: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    account: Option<String>,
    last_ts: i64,
    reset: bool,
    /// Whether records older than `rolled_until` are skipped: the file may repeat rolled-up
    /// ones (`marks::Marks::applies`).
    marked: bool,
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
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // The live view runs a couple of dozen distinct statements every second (`live.rs`);
        // keep them all prepared instead of the default 16.
        conn.set_prepared_statement_cache_capacity(64);
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // The WAL shrinks back to this after a checkpoint (a compaction or a VACUUM grows it).
        conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
        // Facts migrate, the cache is rebuilt when its version changed; requests read again
        // then follow their seat's login history like any new request (`post_pass`).
        schema::migrate(&conn)?;
        Ok(Index {
            conn,
            session_dirs: HashMap::new(),
            dated: None,
            settled: Default::default(),
        })
    }

    pub fn meta(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(schema::meta(&self.conn, key)?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        Ok(schema::set_meta(&self.conn, key, value)?)
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
        self.refresh_files(all, seats, seat_info, resolver, prices, progress)
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
        self.refresh_files(out, seats, seat_info, resolver, prices, &mut quiet)
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
        seats: &[Seat],
        seat_info: &[SeatInfo],
        resolver: &mut Resolver,
        prices: &PriceBook,
        progress: &mut dyn FnMut(Progress),
    ) -> anyhow::Result<RefreshStats> {
        let started = Instant::now();
        let files_total = all.len();
        let work = self.plan(&all, seats)?;
        let from_start = work.iter().any(|w| w.start == 0);
        let mut stats = self.ingest(work, seat_info, resolver, prices, progress)?;
        if from_start {
            // A seat found after a compaction: its old records go straight into the archive.
            self.roll_late(prices, seats)?;
        }
        stats.files_total = files_total;
        stats.elapsed = started.elapsed();
        Ok(stats)
    }

    /// What to read of each file: from the stored offset when the file only grew, from the start
    /// when it was replaced or shrank (`reset`), nothing when it is unchanged. A file read from
    /// its start gets its read marks here, before anything of this refresh is written.
    fn plan(&self, all: &[TranscriptFile], seats: &[Seat]) -> anyhow::Result<Vec<Work>> {
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
        let mut marks: Option<marks::Marks> = None;
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
            let expected = known.get(&key).map(|(kino, _, koff, ..)| (*kino, *koff));
            let (start, account, last_ts, reset) = match known.get(&key) {
                Some((kino, ksize, ..)) if *kino == ino && *ksize == size => continue,
                Some((kino, _, koff, kacc, kts)) if *kino == ino && size >= *koff => {
                    (*koff, kacc.clone(), *kts, false)
                }
                Some(_) => (0, None, 0, true),
                None => (0, None, 0, false),
            };
            if start == 0 && marks.is_none() {
                marks = Some(marks::Marks::load(&self.conn, seats)?);
            }
            let marked = match &marks {
                Some(m) => m.applies(&self.conn, f, start)?,
                None => false,
            };
            work.push(Work {
                file: f.clone(),
                expected,
                start,
                ino,
                size,
                mtime,
                account,
                last_ts,
                reset,
                marked,
            });
        }
        Ok(work)
    }

    /// Reads the planned work and writes it, one immediate transaction per batch: the write lock
    /// is taken before the offsets are checked again, so two processes (two dashboards, a CLI run)
    /// never both add the same bytes.
    fn ingest(
        &mut self,
        mut work: Vec<Work>,
        seat_info: &[SeatInfo],
        resolver: &mut Resolver,
        prices: &PriceBook,
        progress: &mut dyn FnMut(Progress),
    ) -> anyhow::Result<RefreshStats> {
        let bytes_total: u64 = work.iter().map(|w| w.size - w.start).sum();
        let files_total = work.len();
        let mut stats = RefreshStats {
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
        // Rows added by this refresh have a larger rowid: rowids are never given out twice, even
        // after the compactor deleted rows (`compact::requests_rowid_high`).
        let last_rowid = compact::requests_rowid_high(&self.conn)?;
        let mut cwds: std::collections::HashSet<String> = std::collections::HashSet::new();
        let settled_before = ms(Utc::now()) - SETTLED_AFTER_MS;
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
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut late = false;
            let mut skipped = 0;
            let mut rowid = compact::requests_rowid_high(&tx)?;
            // Read in the write transaction: a compaction committed since the plan counts.
            let rolled_until = archive::rolled_until(&tx)?;
            for (w, chunk) in parsed {
                let Ok(chunk) = chunk else { continue };
                let now: Option<(u64, u64)> = tx
                    .query_row(
                        "SELECT ino, offset FROM files WHERE path = ?1",
                        [w.file.path.to_string_lossy()],
                        |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
                    )
                    .optional()?;
                if now != w.expected {
                    // Another process read this file meanwhile; what it wrote stands.
                    continue;
                }
                done.bytes_done += chunk.end - w.start;
                let at = write::At {
                    settled_before,
                    rolled_until,
                };
                let wrote = write::write_file(&tx, w, chunk, &mut cwds, at, &mut rowid)?;
                late |= wrote.late;
                skipped += wrote.skipped;
            }
            if late {
                bump_settled(&tx)?;
            }
            if skipped > 0 {
                // A running count for `doctor`: records found again after they were rolled up.
                tx.execute(
                    "INSERT INTO meta(key, value) VALUES ('skipped_records', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + excluded.value",
                    [skipped as i64],
                )?;
                stats.records_skipped += skipped;
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
            &cwds,
            (files_total > 0).then_some(last_rowid),
        )?;
        let after: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM requests", [], |r| r.get(0))?;
        stats.requests_new = (after - before).max(0) as u64;
        self.set_meta("refreshed_at", &Utc::now().timestamp_millis().to_string())?;
        Ok(stats)
    }

    fn post_pass(
        &mut self,
        seat_info: &[SeatInfo],
        resolver: &mut Resolver,
        prices: &PriceBook,
        new_cwds: &std::collections::HashSet<String>,
        new_after_rowid: Option<i64>,
    ) -> anyhow::Result<()> {
        let now = ms(Utc::now());
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
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
                    // A reading that says what one a few minutes away already said (same account,
                    // window and value) adds nothing: sources report every few seconds.
                    tx.execute(
                        "INSERT INTO readings(account_uuid, kind, fetched_at, utilization, resets_at)
                         SELECT ?1, ?2, ?3, ?4, ?5 WHERE NOT EXISTS (
                           SELECT 1 FROM readings WHERE account_uuid = ?1 AND kind = ?2
                             AND fetched_at BETWEEN ?3 - ?6 AND ?3 + ?6
                             AND utilization = ?4 AND ABS(resets_at - ?5) < 60000)
                         ON CONFLICT(account_uuid, kind, fetched_at) DO NOTHING",
                        params![
                            u.account_uuid,
                            r.kind.key(),
                            ms(r.fetched_at),
                            r.utilization,
                            ms(r.resets_at),
                            READING_REPEAT_MS
                        ],
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
        let mut moved = 0;
        let rolled_until = archive::rolled_until(&tx)?;
        for (seat, ts) in &reattribute {
            moved += tx.execute(
                &format!("UPDATE requests SET account_uuid = {SEAT_ACCOUNT_AT} WHERE seat = ?1 AND ts >= ?2"),
                params![seat, ts],
            )?;
            // A switch older than the raw window (swarmy was not running): the rollups move too.
            if *ts < rolled_until {
                let (insert, delete) = archive::rekey_sql();
                tx.execute(&insert, params![seat, ts])?;
                moved += tx.execute(&delete, params![seat, ts])?;
            }
        }
        if moved > 0 {
            bump_settled(&tx)?;
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
        // Projects of new working directories; a new rule revision recomputes every cwd, raw or
        // rolled up, never downgrading one whose dir is gone (`projects`).
        let cwds: Vec<String> = if rev_ok {
            let mut known_cwd = tx.prepare("SELECT 1 FROM projects WHERE cwd = ?1")?;
            new_cwds
                .iter()
                .filter(|c| !known_cwd.exists([c.as_str()]).unwrap_or(false))
                .cloned()
                .collect()
        } else {
            projects::every_cwd(&tx)?
        };
        projects::assign(&tx, resolver, &cwds)?;
        if !rev_ok {
            schema::set_meta(&tx, "projects_rev", PROJECTS_REV)?;
        }
        tx.commit()?;
        self.price_all(prices)?;
        Ok(())
    }
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
