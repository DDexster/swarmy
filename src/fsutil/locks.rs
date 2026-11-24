//! Which lock files of a directory another process holds, without taking a lock of ours where
//! the OS can say it otherwise. Antigravity keeps `presence/<conversation>.lock` and Codex
//! `thread-writer-locks/<thread>.lock` locked while a conversation runs; both readers ask here.
//!
//! - The directory is listed again only when its mtime changes.
//! - On Linux, which files are held comes from one read of `/proc/locks` (no lock of ours ever
//!   touches the harness's files); elsewhere each file is probed with a shared lock on a
//!   read-only handle.

use std::collections::HashMap;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A lock file: its name without `.lock` and, on Unix, the (device, inode) that `/proc/locks`
/// names it by.
#[derive(Debug, Clone)]
struct LockFile {
    name: String,
    #[cfg_attr(
        not(target_os = "linux"),
        allow(dead_code, reason = "only Linux names locks by (device, inode)")
    )]
    id: Option<(u64, u64)>,
}

/// Every held lock: ((major, minor), inode) of the locked file → the holder's pid.
#[cfg(any(target_os = "linux", test))]
type Held = HashMap<((u64, u64), u64), u32>;

/// Directory listings kept by mtime, so a per-second check lists nothing while nothing changed.
#[derive(Debug, Default)]
pub struct LockDirs {
    dirs: HashMap<PathBuf, (Option<SystemTime>, Vec<LockFile>)>,
}

impl LockDirs {
    /// The names (without `.lock`) of the lock files in `dir` that some process holds, sorted.
    pub fn held(&mut self, dir: &Path) -> Vec<String> {
        let mtime = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        let files = match self.dirs.get(dir) {
            Some((prev, list)) if *prev == mtime && mtime.is_some() => list.clone(),
            Some(_) | None => {
                let list = list_lock_files(dir);
                self.dirs.insert(dir.to_path_buf(), (mtime, list.clone()));
                list
            }
        };
        held(dir, &files)
    }
}

/// One check without a cache (CLI commands).
pub fn held_once(dir: &Path) -> Vec<String> {
    held(dir, &list_lock_files(dir))
}

fn list_lock_files(dir: &Path) -> Vec<LockFile> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<LockFile> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // `.coordination.lock` and other hidden files are no conversation.
            if name.starts_with('.') {
                return None;
            }
            let name = name.strip_suffix(".lock")?.to_string();
            Some(LockFile {
                name,
                id: file_id(&e.path()),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
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

/// The files someone holds a lock on.
fn held(dir: &Path, files: &[LockFile]) -> Vec<String> {
    #[cfg(target_os = "linux")]
    if let Some(locks) = proc_locks() {
        return files
            .iter()
            .filter(|f| {
                f.id.is_some_and(|(dev, ino)| locks.contains_key(&(dev_parts(dev), ino)))
            })
            .map(|f| f.name.clone())
            .collect();
    }
    files
        .iter()
        .filter(|f| is_locked(&dir.join(format!("{}.lock", f.name))))
        .map(|f| f.name.clone())
        .collect()
}

/// The processes holding a lock on each named file of `dir` (Linux only: `/proc/locks` names
/// the holder; elsewhere empty).
pub fn holders(dir: &Path, names: &[String]) -> HashMap<String, u32> {
    #[cfg(target_os = "linux")]
    if let Some(locks) = proc_locks() {
        return names
            .iter()
            .filter_map(|n| {
                let (dev, ino) = file_id(&dir.join(format!("{n}.lock")))?;
                let pid = *locks.get(&(dev_parts(dev), ino))?;
                Some((n.clone(), pid))
            })
            .collect();
    }
    let _ = (dir, names);
    HashMap::new()
}

/// Linux's split of a `st_dev` into (major, minor), as `/proc/locks` prints them.
#[cfg(target_os = "linux")]
fn dev_parts(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major, minor)
}

/// Every lock in `/proc/locks` as ((major, minor), inode) → holder pid; `None` when it cannot
/// be read.
#[cfg(target_os = "linux")]
fn proc_locks() -> Option<Held> {
    parse_proc_locks(&std::fs::read_to_string("/proc/locks").ok()?)
}

/// Lines look like `90: FLOCK  ADVISORY  WRITE 1060823 103:02:60181261 0 EOF` (major and
/// minor in hex); a blocked waiter (`-> FLOCK …`) holds nothing.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_locks(text: &str) -> Option<Held> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let (Some(_), Some(kind)) = (f.next(), f.next()) else {
            continue;
        };
        if kind == "->" {
            continue;
        }
        let (Some(_), Some(_), Some(pid), Some(dev_ino)) = (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let mut parts = dev_ino.split(':');
        let (Some(maj), Some(min), Some(ino)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(maj), Ok(min), Ok(ino), Ok(pid)) = (
            u64::from_str_radix(maj, 16),
            u64::from_str_radix(min, 16),
            ino.parse::<u64>(),
            pid.parse::<u32>(),
        ) else {
            continue;
        };
        out.insert(((maj, min), ino), pid);
    }
    Some(out)
}

/// True when another process holds an exclusive lock on `path` (the probe where `/proc/locks`
/// is not there). Uses a read-only file handle and `File::try_lock_shared()` so swarmy never
/// opens a harness's files for writing.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_held_locks_and_their_holders_from_proc_locks() {
        let text = "1: POSIX  ADVISORY  WRITE 3178 103:02:59507537 0 0\n\
                    90: FLOCK  ADVISORY  WRITE 1060823 103:02:60181261 0 EOF\n\
                    90: -> FLOCK  ADVISORY  WRITE 1060999 103:02:77 0 EOF\n";
        let locks = parse_proc_locks(text).unwrap();
        assert_eq!(locks.get(&((0x103, 2), 60181261)), Some(&1060823));
        assert_eq!(locks.get(&((0x103, 2), 59507537)), Some(&3178));
        assert!(
            !locks.contains_key(&((0x103, 2), 77)),
            "a waiter holds nothing"
        );
        assert_eq!(locks.len(), 2);
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
    fn tells_held_lock_files_from_free_ones() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("locks");
        std::fs::create_dir_all(&dir).unwrap();
        let live_path = dir.join("conv-live.lock");
        std::fs::write(&live_path, "").unwrap();
        std::fs::write(dir.join("conv-idle.lock"), "").unwrap();
        std::fs::write(dir.join(".coordination.lock"), "").unwrap();

        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&live_path)
            .unwrap();
        holder.try_lock().unwrap();

        let mut dirs = LockDirs::default();
        assert_eq!(dirs.held(&dir), vec!["conv-live"]);
        assert_eq!(held_once(&dir), vec!["conv-live"]);
        #[cfg(target_os = "linux")]
        assert_eq!(
            holders(&dir, &["conv-live".to_string()]).get("conv-live"),
            Some(&std::process::id())
        );

        holder.unlock().unwrap();
        drop(holder);
        assert!(dirs.held(&dir).is_empty());
    }
}
