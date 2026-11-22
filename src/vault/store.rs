//! Secret storage and safe file writes.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde_json::Value;

use crate::claude::credentials::Secret;

pub trait SecretStore {
    fn name(&self) -> &'static str;
    fn get(&self, id: &str) -> anyhow::Result<Option<Secret>>;
    fn put(&self, id: &str, secret: &Secret) -> anyhow::Result<()>;
    fn delete(&self, id: &str) -> anyhow::Result<()>;
}

/// A JSON file readable only by the owner.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: PathBuf) -> FileStore {
        FileStore { path }
    }

    fn load(&self) -> anyhow::Result<BTreeMap<String, Value>> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => serde_json::from_str(&t).context("vault/secrets.json is damaged"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.into()),
        }
    }
}

impl SecretStore for FileStore {
    fn name(&self) -> &'static str {
        "file (0600)"
    }

    fn get(&self, id: &str) -> anyhow::Result<Option<Secret>> {
        Ok(self.load()?.remove(id).map(Secret::new))
    }

    fn put(&self, id: &str, secret: &Secret) -> anyhow::Result<()> {
        let mut m = self.load()?;
        m.insert(id.to_string(), secret.expose().clone());
        atomic_write(&self.path, serde_json::to_string(&m)?.as_bytes())
    }

    fn delete(&self, id: &str) -> anyhow::Result<()> {
        let mut m = self.load()?;
        if m.remove(id).is_some() {
            atomic_write(&self.path, serde_json::to_string(&m)?.as_bytes())?;
        }
        Ok(())
    }
}

/// Writes through a temporary file in the same directory, fsyncs, and renames over the target, so
/// a crash leaves either the old or the new file, never half of one. Mode 0600 on Unix.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().context("path without a parent")?;
    let name = path
        .file_name()
        .context("path without a file name")?
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}.swarmy-{}.tmp", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("cannot replace {}", path.display()));
    }
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
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

/// Keeps the newest `keep` backups.
pub fn prune_backups(dir: &Path, keep: usize) -> anyhow::Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    if files.len() > keep {
        for f in &files[..files.len() - keep] {
            let _ = std::fs::remove_file(f);
        }
    }
    Ok(())
}

/// An exclusive lock file holding the pid; a lock left by a dead process is taken over.
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn acquire(path: &Path) -> anyhow::Result<Lock> {
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut f) => {
                    write!(f, "{}", std::process::id())?;
                    return Ok(Lock {
                        path: path.to_path_buf(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let pid: Option<u32> = std::fs::read_to_string(path)
                        .ok()
                        .and_then(|s| s.trim().parse().ok());
                    let alive =
                        pid.is_some_and(|p| crate::claude::procs::ProcTable::snapshot().alive(p));
                    if alive {
                        bail!(
                            "another swarmy is changing the vault (pid {})",
                            pid.unwrap()
                        );
                    }
                    let _ = std::fs::remove_file(path);
                }
                Err(e) => return Err(e.into()),
            }
        }
        bail!("cannot lock {}", path.display())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
