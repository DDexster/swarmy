//! The vault's files: the exclusive lock, the secrets file, safe writes and bounded backups.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::fsutil::{self, Mode};

/// Secrets by login id: the verbatim `claudeAiOauth` objects.
pub type Secrets = BTreeMap<String, Value>;

/// Where secrets are kept: a JSON file readable only by the owner.
pub const STORE_NAME: &str = "file (0600)";

/// Reads a file that may not exist yet.
pub fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Writes through a unique temp file, fsyncs it and the directory, mode 0600 on Unix
/// (`fsutil::atomic_write`, the one implementation).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    fsutil::atomic_write(path, bytes, Mode::Private)
        .with_context(|| format!("cannot write {}", path.display()))
}

/// What identifies one version of a file: another writer's rename or rewrite changes it.
#[derive(PartialEq)]
struct Stamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    #[cfg(unix)]
    ino: u64,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some(Stamp {
        modified: m.modified().ok(),
        len: m.len(),
        #[cfg(unix)]
        ino: std::os::unix::fs::MetadataExt::ino(&m),
    })
}

/// Rewrites a file that another program (Claude Code) also rewrites: reads it, applies `change`
/// to the text, writes the result to a temp file and renames it over the target only if the file
/// has not changed since it was read; otherwise starts again from the newer text. Returns false
/// (and writes nothing) when the file does not exist.
pub fn rewrite_merging(
    path: &Path,
    change: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<bool> {
    const ATTEMPTS: usize = 8;
    for attempt in 1..=ATTEMPTS {
        let before = stamp(path);
        let Some(text) = read_optional(path)? else {
            return Ok(false);
        };
        let staged = fsutil::stage(path, change(&text)?.as_bytes(), Mode::Private)
            .with_context(|| format!("cannot write next to {}", path.display()))?;
        // The last attempt writes anyway: the window between this check and the rename is tiny.
        if stamp(path) == before || attempt == ATTEMPTS {
            staged
                .commit(path)
                .with_context(|| format!("cannot replace {}", path.display()))?;
            return Ok(true);
        }
        // Dropped: the temp file is removed, and the next attempt starts from the newer text.
        drop(staged);
    }
    Ok(false)
}

pub fn restrict_dir(dir: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _ = dir;
    Ok(())
}

/// A backup stamp, sortable as text, free in `dir` for every `{stamp}-{kind}` name.
fn unique_stamp(dir: &Path, kinds: &[&str]) -> String {
    let mut t: DateTime<Utc> = Utc::now();
    loop {
        let s = t.format("%Y%m%dT%H%M%S%.6f").to_string();
        if kinds.iter().all(|k| !dir.join(format!("{s}-{k}")).exists()) {
            return s;
        }
        t += chrono::Duration::microseconds(1);
    }
}

/// Keeps the newest `keep` files of one kind (`*-{kind}`) in `dir`.
fn prune_kind(dir: &Path, kind: &str, keep: usize) -> anyhow::Result<()> {
    let suffix = format!("-{kind}");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
        })
        .collect();
    files.sort();
    if files.len() > keep {
        for f in &files[..files.len() - keep] {
            std::fs::remove_file(f).with_context(|| format!("cannot remove {}", f.display()))?;
        }
    }
    Ok(())
}

/// Saves copies of several files under one stamp in `dir` (`{stamp}-{kind}`; `None` texts are
/// skipped), then keeps the newest `keep` of each kind. Names never collide, even within the same
/// second; callers hold the vault lock, so no other swarmy writes there meanwhile.
pub fn backup(dir: &Path, files: &[(&str, Option<&str>)], keep: usize) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let kinds: Vec<&str> = files.iter().map(|(k, _)| *k).collect();
    let stamp = unique_stamp(dir, &kinds);
    for (kind, text) in files {
        if let Some(t) = text {
            atomic_write(&dir.join(format!("{stamp}-{kind}")), t.as_bytes())?;
        }
    }
    for kind in kinds {
        prune_kind(dir, kind, keep)?;
    }
    Ok(())
}

/// An exclusive OS lock on `vault/lock` (`flock` on Unix). The kernel releases it when the file
/// is closed or the process dies, so there is no stale lock to take over. Locks belong to the open
/// file, so two threads of one process exclude each other too.
pub struct Lock {
    _file: std::fs::File,
}

impl Lock {
    /// Waits up to `timeout` for the lock.
    pub fn acquire(path: &Path, timeout: Duration) -> anyhow::Result<Lock> {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Lock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(crate::errors::VaultError::Locked(timeout).into());
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(e).with_context(|| format!("cannot lock {}", path.display()));
                }
            }
        }
    }
}
