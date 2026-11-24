//! Live session detection for Google Antigravity CLI (`agy`).
//!
//! Every running `agy` instance holds an exclusive OS lock on
//! `<seat>/presence/<conversation_id>.lock`. Checking `File::try_lock_shared()` on a read-only
//! handle detects live conversations without ever modifying any file.
//!
//! On Linux, inspecting `/proc/<pid>/fd` and `/proc/<pid>/environ` for `agy` processes maps each
//! locked conversation to its exact PID, working directory and `TMUX_PANE` (for `J` / `swarmy jump`
//! and CPU/RAM stats). Process discovery is cached by locked conversation ID so per-second ticks
//! never scan `/proc`.

use std::collections::{HashMap, HashSet};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use super::meta::{ConversationMeta, RunStatus};
use crate::claude::procs::ProcTable;
use crate::claude::registry::{LiveSession, SessionStatus};
use crate::core::harness::Harness;
use crate::core::ids::{SeatId, SessionId};

/// Cached process metadata for one live Antigravity conversation (Linux `/proc`).
#[derive(Debug, Clone)]
pub struct AgyProcInfo {
    pub pid: u32,
    pub cwd: Option<String>,
    /// Where it runs, for `J`: its tmux pane, else the terminal its environment names.
    pub pane: Option<crate::core::pane::Pane>,
}

/// A lock file of `presence/`: its conversation id and, on Unix, the (device, inode) that
/// `/proc/locks` names it by.
#[derive(Debug, Clone)]
struct LockFile {
    cid: String,
    id: Option<(u64, u64)>,
}

/// Per-seat cache keeping per-second `agy` live detection inside the battery budget:
/// - `presence/` is listed again only when its mtime changes;
/// - on Linux, which lock files are held comes from one read of `/proc/locks` (no lock of ours
///   ever touches Antigravity's files); elsewhere each file is probed with a shared lock;
/// - `/proc` is scanned for `agy` PIDs only when the set of locked conversations changed and one
///   of them has no live PID yet, so a process that cannot be found costs one scan, not one a
///   second.
#[derive(Debug, Default)]
pub struct AgyLiveCache {
    presence_dirs: HashMap<PathBuf, (Option<SystemTime>, Vec<LockFile>)>,
    by_conv: HashMap<String, AgyProcInfo>,
    /// The locked conversations the last `/proc` scan was made for, per seat dir.
    scanned_for: HashMap<PathBuf, Vec<String>>,
}

impl AgyLiveCache {
    /// Conversation IDs whose `presence/<cid>.lock` is currently locked by a running `agy`.
    pub fn locked_conversations(&mut self, seat_dir: &Path) -> Vec<String> {
        let dir = seat_dir.join("presence");
        let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
        let files = match self.presence_dirs.get(&dir) {
            Some((prev, list)) if *prev == mtime && mtime.is_some() => list.clone(),
            Some(_) | None => {
                let list = list_lock_files(&dir);
                self.presence_dirs
                    .insert(dir.clone(), (mtime, list.clone()));
                list
            }
        };
        held(&dir, &files)
    }

    /// PIDs currently cached for live `agy` conversations (for `ProcTable::for_pids`).
    pub fn tracked_pids(&self) -> Vec<u32> {
        self.by_conv.values().map(|p| p.pid).collect()
    }

    /// Resolves PID / CWD / TMUX_PANE for `locked` conversations. `/proc` is scanned only when
    /// a locked conversation has no live PID and the locked set is not the one already scanned
    /// for.
    pub fn resolve_procs(&mut self, seat_dir: &Path, locked: &[String]) {
        if locked.is_empty() {
            return;
        }
        let locked_set: HashSet<&str> = locked.iter().map(String::as_str).collect();
        self.by_conv
            .retain(|cid, info| locked_set.contains(cid.as_str()) && pid_is_alive(info.pid));
        let missing = locked.iter().any(|cid| !self.by_conv.contains_key(cid));
        let mut key = locked.to_vec();
        key.sort();
        if missing && self.scanned_for.get(seat_dir) != Some(&key) {
            for (cid, info) in scan_agy_procs(seat_dir) {
                if locked_set.contains(cid.as_str()) {
                    self.by_conv.insert(cid, info);
                }
            }
            self.scanned_for.insert(seat_dir.to_path_buf(), key);
        }
    }

    pub fn proc_of(&self, cid: &str) -> Option<&AgyProcInfo> {
        self.by_conv.get(cid)
    }
}

/// Stateless one-shot helper for CLI commands (`doctor`, `jump`).
pub fn locked_conversations_once(seat_dir: &Path) -> Vec<String> {
    let dir = seat_dir.join("presence");
    held(&dir, &list_lock_files(&dir))
}

fn list_lock_files(presence_dir: &Path) -> Vec<LockFile> {
    let Ok(rd) = std::fs::read_dir(presence_dir) else {
        return Vec::new();
    };
    let mut out: Vec<LockFile> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let cid = name.strip_suffix(".lock")?.to_string();
            Some(LockFile {
                cid,
                id: file_id(&e.path()),
            })
        })
        .collect();
    out.sort_by(|a, b| a.cid.cmp(&b.cid));
    out
}

#[cfg(unix)]
fn file_id(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_p: &Path) -> Option<(u64, u64)> {
    None
}

/// The conversations whose lock file someone holds.
fn held(presence_dir: &Path, files: &[LockFile]) -> Vec<String> {
    #[cfg(target_os = "linux")]
    if let Some(locks) = proc_locks() {
        return files
            .iter()
            .filter(|f| {
                f.id.is_some_and(|(dev, ino)| locks.contains(&(dev_parts(dev), ino)))
            })
            .map(|f| f.cid.clone())
            .collect();
    }
    files
        .iter()
        .filter(|f| is_locked(&presence_dir.join(format!("{}.lock", f.cid))))
        .map(|f| f.cid.clone())
        .collect()
}

/// Linux's split of a `st_dev` into (major, minor), as `/proc/locks` prints them.
#[cfg(target_os = "linux")]
fn dev_parts(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major, minor)
}

/// Every lock in `/proc/locks` as ((major, minor), inode); `None` when it cannot be read.
#[cfg(target_os = "linux")]
fn proc_locks() -> Option<HashSet<((u64, u64), u64)>> {
    parse_proc_locks(&std::fs::read_to_string("/proc/locks").ok()?)
}

/// Lines look like `90: FLOCK  ADVISORY  WRITE 1060823 103:02:60181261 0 EOF` (major and
/// minor in hex); a blocked waiter (`-> FLOCK …`) holds nothing.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_locks(text: &str) -> Option<HashSet<((u64, u64), u64)>> {
    let mut out = HashSet::new();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let (Some(_), Some(kind)) = (f.next(), f.next()) else {
            continue;
        };
        if kind == "->" {
            continue;
        }
        let Some(dev_ino) = f.nth(3) else { continue };
        let mut parts = dev_ino.split(':');
        let (Some(maj), Some(min), Some(ino)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(maj), Ok(min), Ok(ino)) = (
            u64::from_str_radix(maj, 16),
            u64::from_str_radix(min, 16),
            ino.parse::<u64>(),
        ) else {
            continue;
        };
        out.insert(((maj, min), ino));
    }
    Some(out)
}

/// True when another process holds an exclusive lock on `path` (the probe where `/proc/locks`
/// is not there). Uses a read-only file handle and `File::try_lock_shared()` so swarmy never
/// opens `agy` files for writing.
pub fn is_locked(path: &Path) -> bool {
    let Ok(f) = File::open(path) else {
        return false;
    };
    match f.try_lock_shared() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

/// Builds normalized [`LiveSession`]s from locked conversations, their `conversation_summaries`
/// metadata, and cached process info.
pub fn build_live_sessions(
    seat: &SeatId,
    locked: &[String],
    metas: &HashMap<String, ConversationMeta>,
    cache: &AgyLiveCache,
    procs: &ProcTable,
    now: DateTime<Utc>,
) -> Vec<LiveSession> {
    let mut out = Vec::new();
    for cid in locked {
        let meta = metas.get(cid);
        let proc_info = cache.proc_of(cid);
        let pid = proc_info.map(|p| p.pid).unwrap_or(0);
        let cwd = meta
            .and_then(|m| m.cwd.clone())
            .or_else(|| proc_info.and_then(|p| p.cwd.clone()))
            .unwrap_or_default();
        let status = match meta.map(|m| m.status).unwrap_or(RunStatus::Idle) {
            RunStatus::Running => SessionStatus::Busy,
            RunStatus::WaitingForUser => SessionStatus::Waiting("input needed".into()),
            RunStatus::Idle => SessionStatus::Idle,
        };
        let started_at = if pid > 0
            && let Some(p) = procs.agy.iter().find(|p| p.pid == pid)
            && let Some(dt) = DateTime::<Utc>::from_timestamp(p.start_time as i64, 0)
        {
            dt
        } else {
            meta.and_then(|m| m.last_modified).unwrap_or(now)
        };
        out.push(LiveSession {
            seat: seat.clone(),
            harness: Harness::Agy,
            pid,
            session_id: SessionId::new(cid.clone()),
            cwd,
            name: meta.and_then(|m| m.title.clone()),
            status,
            pane: proc_info.and_then(|p| p.pane.clone()),
            version: None,
            kind: Some("agy".into()),
            started_at,
            status_since: meta.and_then(|m| m.last_modified),
        });
    }
    out.sort_by_key(|s| s.started_at);
    out
}

fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Scans `/proc/<pid>` on Linux for running `agy` processes that hold a `presence/<cid>.lock`
/// file descriptor inside `seat_dir`, extracting their `cwd` and `TMUX_PANE`.
#[cfg(target_os = "linux")]
fn scan_agy_procs(seat_dir: &Path) -> HashMap<String, AgyProcInfo> {
    let mut out = HashMap::new();
    let presence_prefix = crate::fsutil::canonical(seat_dir)
        .unwrap_or_else(|_| seat_dir.to_path_buf())
        .join("presence");
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let pdir = entry.path();
        let Ok(comm) = std::fs::read_to_string(pdir.join("comm")) else {
            continue;
        };
        if comm.trim() != "agy" {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(pdir.join("fd")) else {
            continue;
        };
        let mut cids = Vec::new();
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            if let Ok(rel) = target.strip_prefix(&presence_prefix)
                && let Some(fname) = rel.file_name().and_then(|n| n.to_str())
                && let Some(cid) = fname.strip_suffix(".lock")
            {
                cids.push(cid.to_string());
            }
        }
        if cids.is_empty() {
            continue;
        }
        let cwd = std::fs::read_link(pdir.join("cwd"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        let pane = crate::claude::pane::of(read_tmux_pane(&pdir.join("environ")), pid, None);
        for cid in cids {
            out.insert(
                cid,
                AgyProcInfo {
                    pid,
                    cwd: cwd.clone(),
                    pane: pane.clone(),
                },
            );
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn read_tmux_pane(environ_path: &Path) -> Option<String> {
    let bytes = std::fs::read(environ_path).ok()?;
    let mut has_tmux = false;
    let mut pane = None;
    for entry in bytes.split(|b| *b == 0) {
        if entry.starts_with(b"TMUX=") && entry.len() > 5 {
            has_tmux = true;
        } else if let Some(rest) = entry.strip_prefix(b"TMUX_PANE=")
            && !rest.is_empty()
        {
            pane = Some(String::from_utf8_lossy(rest).into_owned());
        }
    }
    has_tmux.then_some(pane).flatten()
}

#[cfg(not(target_os = "linux"))]
fn scan_agy_procs(_seat_dir: &Path) -> HashMap<String, AgyProcInfo> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_held_locks_from_proc_locks() {
        let text = "1: POSIX  ADVISORY  WRITE 3178 103:02:59507537 0 0\n\
                    90: FLOCK  ADVISORY  WRITE 1060823 103:02:60181261 0 EOF\n\
                    90: -> FLOCK  ADVISORY  WRITE 1060999 103:02:77 0 EOF\n";
        let locks = parse_proc_locks(text).unwrap();
        assert!(locks.contains(&((0x103, 2), 60181261)));
        assert!(locks.contains(&((0x103, 2), 59507537)));
        assert!(!locks.contains(&((0x103, 2), 77)), "a waiter holds nothing");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn splits_a_linux_device_number() {
        // makedev(259, 2) = 0x10302
        assert_eq!(dev_parts(0x10302), (259, 2));
        // makedev(8, 1) = 0x801
        assert_eq!(dev_parts(0x801), (8, 1));
    }

    #[test]
    fn detects_locked_vs_unlocked_presence_files() {
        let t = tempfile::tempdir().unwrap();
        let pres = t.path().join("presence");
        std::fs::create_dir_all(&pres).unwrap();
        let live_path = pres.join("conv-live.lock");
        let idle_path = pres.join("conv-idle.lock");
        std::fs::write(&live_path, "").unwrap();
        std::fs::write(&idle_path, "").unwrap();

        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&live_path)
            .unwrap();
        holder.try_lock().unwrap();

        let mut cache = AgyLiveCache::default();
        assert_eq!(cache.locked_conversations(t.path()), vec!["conv-live"]);

        holder.unlock().unwrap();
        drop(holder);
        assert!(cache.locked_conversations(t.path()).is_empty());
    }
}
