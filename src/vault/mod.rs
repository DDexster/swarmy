//! The login vault: saved Claude Code OAuth logins, and switching which one a seat uses.
//!
//! A *login* is the `claudeAiOauth` object of a credentials file (an OAuth grant) plus facts
//! about it, named by the fingerprint of its refresh token. A *seat* is a Claude Code config dir.
//! Claude Code refreshes the access token every few hours and keeps the refresh token (verified
//! on 2.1.329), so a saved copy's access token goes stale while the login is in use: the vault
//! tracks which seat each login is mounted in and, before every change, takes the seat's
//! current tokens back first. A refresh token that rotates anyway is followed explicitly
//! (`VaultTx::sync_seat`), so the login keeps its identity.
//!
//! Secrets live in `vault/secrets.json` with mode 0600 — the same protection Claude Code gives the
//! very same tokens in `.credentials.json` on Linux and Windows.
//!
//! Reading needs no lock: every file is replaced by an atomic rename, so a reader sees a whole
//! old or a whole new file. Every change goes through [`Vault::edit`], which holds the vault's
//! OS lock, re-reads the files under it, and saves them through a [`VaultTx`].

mod logins;
mod seats;
mod store;
mod switch;

use logins::{Index, read_index};
pub use switch::SwitchReport;

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude::config_json;
use crate::claude::credentials::{self, LoginMeta, Secret};
use crate::claude::seats::{Seat, SeatId};
pub use crate::errors::VaultError;
use store::{Secrets, atomic_write};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Login {
    pub id: String,
    pub label: String,
    pub account_uuid: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub meta: LoginMeta,
    /// `oauthAccount` from the seat's `.claude.json` at capture time, restored on a switch.
    pub oauth_account: Option<Value>,
    pub captured_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The seat this login is currently written into.
    pub mounted: Option<SeatId>,
    /// A seat label an older swarmy stored in `mounted`, not matched to a seat yet
    /// (`Index::adopt_seats`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounted_label: Option<String>,
}

/// What a seat holds, as the vault sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeatState {
    pub fingerprint: Option<String>,
    /// The account from Claude Code's profile (`<accountUuid>/<organizationUuid>` when known),
    /// when it is known to belong to the current login.
    pub account_uuid: Option<String>,
    /// The login changed after the profile was read: the account behind it is not known yet.
    pub unverified_since: Option<DateTime<Utc>>,
    /// When the vault last switched this seat's login.
    pub switched_at: Option<DateTime<Utc>>,
    /// The saved login that holds exactly these tokens.
    pub saved_as: Option<String>,
}

/// How long a change waits for another swarmy (process or thread) to finish its own.
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Copies kept of each of the vault's own files, and of each seat file per seat.
const VAULT_BACKUPS: usize = 30;
const SEAT_BACKUPS: usize = 20;
/// A read-only view of the vault, as of the last `open` or `edit`.
pub struct Vault {
    dir: PathBuf,
    index: Index,
}

/// What a seat holds right now.
#[derive(Debug, Clone)]
pub struct SeatLogin {
    pub secret: Option<Secret>,
    /// When the credentials file was last written.
    pub written_at: Option<DateTime<Utc>>,
    pub oauth_account: Option<Value>,
    pub account: Option<config_json::Account>,
}

pub fn read_seat(seat: &Seat) -> anyhow::Result<SeatLogin> {
    let secret = match std::fs::read_to_string(seat.credentials_file()) {
        Ok(t) => credentials::parse_login(&t)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if cfg!(target_os = "macos") {
                return Err(VaultError::Keychain.into());
            }
            None
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("cannot read {}", seat.credentials_file().display()));
        }
    };
    // A damaged `.claude.json` fails the read: saving a login without its account would label
    // it wrongly later.
    let cj = config_json::read_seat_file(&seat.claude_json)?;
    let mut account = cj.account;
    if let (Some(a), Some(sec)) = (account.as_mut(), secret.as_ref()) {
        a.apply_credentials(&sec.meta());
    }
    let written_at = std::fs::metadata(seat.credentials_file())
        .ok()
        .and_then(|m| m.modified().ok())
        .map(DateTime::<Utc>::from);
    Ok(SeatLogin {
        secret,
        written_at,
        oauth_account: cj.oauth_account_raw,
        account,
    })
}

/// The index as a reader sees it: labels an older version duplicated are told apart the way the
/// next change will save them (`VaultTx::begin`), so a label never names two logins.
fn read_view(dir: &Path) -> anyhow::Result<Index> {
    let (mut index, _) = read_index(dir)?;
    index.dedupe_labels();
    Ok(index)
}

/// One line in `vault/journal.log`, the human-readable history of changes. The change itself
/// is already saved; a journal that cannot be written is reported on the debug channel.
fn journal(dir: &Path, line: &str) {
    use std::io::Write;
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("journal.log"))
        .and_then(|mut f| writeln!(f, "{} {line}", Utc::now().to_rfc3339()));
    if let Err(e) = written {
        crate::diag::log("vault", &format!("journal.log: {e}"));
    }
}

/// A seat label as a directory name.
fn safe_name(s: &str) -> String {
    let n: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if n.trim_matches('.').is_empty() {
        format!("_{n}")
    } else {
        n
    }
}

impl Vault {
    /// Reads the vault without locking it.
    pub fn open(dir: &Path) -> anyhow::Result<Vault> {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        store::restrict_dir(dir)?;
        let index = read_view(dir)?;
        Ok(Vault {
            dir: dir.to_path_buf(),
            index,
        })
    }

    pub fn logins(&self) -> &[Login] {
        &self.index.logins
    }

    /// The login a label, an id or an email names (an error when it names none or several).
    pub fn find(&self, key: &str) -> Result<&Login, VaultError> {
        self.index.find(key)
    }

    pub fn store_name(&self) -> &'static str {
        store::STORE_NAME
    }

    /// The only way to change the vault: takes the exclusive lock, re-reads the files under it
    /// (never a copy read before), runs `f`, saves what changed (after a backup of each changed
    /// file) and releases the lock. When `f` fails, nothing it staged is saved. Afterwards `self`
    /// shows the vault as saved.
    pub fn edit<T>(
        &mut self,
        f: impl FnOnce(&mut VaultTx<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.edit_within(LOCK_TIMEOUT, f)
    }

    /// [`Vault::edit`] when no other swarmy is changing the vault right now, else `None` at once
    /// (nothing waits, nothing is written): for bookkeeping a reader may do on the side.
    pub fn try_edit<T>(
        &mut self,
        f: impl FnOnce(&mut VaultTx<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<Option<T>> {
        match self.edit_within(std::time::Duration::ZERO, f) {
            Ok(v) => Ok(Some(v)),
            Err(e)
                if e.downcast_ref::<VaultError>()
                    .is_some_and(|v| matches!(v, VaultError::Locked(_))) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// What every seat holds, as the vault sees it, without the lock and without writing
    /// anything: for read paths (`Ctx::load`, reports, `doctor`), which must never wait for or
    /// fail on another swarmy's change. The bookkeeping this would do (copies kept current,
    /// rotations followed, first-seen times) is saved only by changes: `swarmy accounts`, a
    /// switch, and the dashboard's [`Vault::try_edit`].
    pub fn seat_states(
        &self,
        seats: &[Seat],
    ) -> anyhow::Result<std::collections::BTreeMap<SeatId, SeatState>> {
        VaultTx::detached(&self.dir)?.seat_states(seats)
    }

    fn edit_within<T>(
        &mut self,
        timeout: std::time::Duration,
        f: impl FnOnce(&mut VaultTx<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let lock = store::Lock::acquire(&self.dir.join("lock"), timeout)?;
        let mut tx = VaultTx::begin(&self.dir, &lock)?;
        let out = f(&mut tx).and_then(|v| tx.save().map(|()| v));
        match &out {
            Ok(_) => self.index = tx.index,
            Err(_) => {
                if let Ok(index) = read_view(&self.dir) {
                    self.index = index;
                }
            }
        }
        out
    }
}

/// A change in progress: the vault as read under the lock, plus what is on disk, so that saving
/// writes only what changed. Only [`Vault::edit`] makes one.
pub struct VaultTx<'a> {
    dir: PathBuf,
    index: Index,
    secrets: Secrets,
    disk_logins: Option<String>,
    disk_secrets: Secrets,
    disk_secrets_text: Option<String>,
    /// `None` only for a read view (`VaultTx::detached`), which cannot save.
    lock: Option<&'a store::Lock>,
}

impl<'a> VaultTx<'a> {
    fn begin(dir: &Path, lock: &'a store::Lock) -> anyhow::Result<VaultTx<'a>> {
        let (mut index, disk_logins) = read_index(dir)?;
        let secrets_path = dir.join("secrets.json");
        let disk_secrets_text = store::read_optional(&secrets_path)?;
        let disk_secrets: Secrets = match &disk_secrets_text {
            Some(t) => serde_json::from_str(t)
                .map_err(|e| anyhow::Error::new(e).context(VaultError::Damaged("secrets.json")))?,
            None => Secrets::new(),
        };
        for (old, new) in index.dedupe_labels() {
            journal(dir, &format!("duplicate label '{old}' renamed to '{new}'"));
        }
        Ok(VaultTx {
            dir: dir.to_path_buf(),
            index,
            secrets: disk_secrets.clone(),
            disk_logins,
            disk_secrets,
            disk_secrets_text,
            lock: Some(lock),
        })
    }

    /// The vault as it is on disk, without the lock and without its secrets: the state for a
    /// read view ([`Vault::seat_states`]). It never saves.
    fn detached(dir: &Path) -> anyhow::Result<VaultTx<'static>> {
        let (mut index, disk_logins) = read_index(dir)?;
        index.dedupe_labels();
        Ok(VaultTx {
            dir: dir.to_path_buf(),
            index,
            secrets: Secrets::new(),
            disk_logins,
            disk_secrets: Secrets::new(),
            disk_secrets_text: None,
            lock: None,
        })
    }

    /// Writes what changed so far. New secrets go to disk before the index that lists them, and
    /// removed ones after the index that no longer does, so a reader never finds a listed login
    /// without its secret. Each changed file is backed up first.
    fn save(&mut self) -> anyhow::Result<()> {
        if self.lock.is_none() {
            anyhow::bail!("a read view of the vault cannot be saved");
        }
        let logins_text = serde_json::to_string_pretty(&self.index)?;
        let logins_changed = self.disk_logins.as_deref() != Some(logins_text.as_str());
        let secrets_changed = self.secrets != self.disk_secrets;
        if !logins_changed && !secrets_changed {
            return Ok(());
        }
        store::backup(
            &self.dir.join("backups").join("vault"),
            &[
                (
                    "logins.json",
                    self.disk_logins.as_deref().filter(|_| logins_changed),
                ),
                (
                    "secrets.json",
                    self.disk_secrets_text
                        .as_deref()
                        .filter(|_| secrets_changed),
                ),
            ],
            VAULT_BACKUPS,
        )?;
        let secrets_path = self.dir.join("secrets.json");
        if secrets_changed {
            let mut grown = self.disk_secrets.clone();
            grown.extend(self.secrets.clone());
            if grown != self.disk_secrets {
                let text = serde_json::to_string(&grown)?;
                atomic_write(&secrets_path, text.as_bytes())?;
                self.disk_secrets = grown;
                self.disk_secrets_text = Some(text);
            }
        }
        if logins_changed {
            atomic_write(&self.dir.join("logins.json"), logins_text.as_bytes())?;
            self.disk_logins = Some(logins_text);
        }
        if self.secrets != self.disk_secrets {
            let text = serde_json::to_string(&self.secrets)?;
            atomic_write(&secrets_path, text.as_bytes())?;
            self.disk_secrets = self.secrets.clone();
            self.disk_secrets_text = Some(text);
        }
        Ok(())
    }

    pub fn find(&self, key: &str) -> Result<&Login, VaultError> {
        self.index.find(key)
    }

    fn secret(&self, id: &str) -> Option<Secret> {
        self.secrets.get(id).cloned().map(Secret::new)
    }

    fn set_secret(&mut self, id: &str, secret: &Secret) {
        self.secrets.insert(id.to_string(), secret.expose().clone());
    }

    fn journal(&self, line: &str) {
        // A read view changes nothing, so it has nothing to tell.
        if self.lock.is_some() {
            journal(&self.dir, line);
        }
    }

    /// `backups/seats/<seat id>`: kept per seat whatever its label. Backups an older swarmy kept
    /// under the seat's label move there first.
    fn seat_backups(&self, seat: &Seat) -> anyhow::Result<PathBuf> {
        let root = self.dir.join("backups").join("seats");
        let dir = root.join(seat.id.as_str());
        let old = root.join(safe_name(&seat.label));
        if !dir.exists() && old.is_dir() {
            crate::fsutil::move_into_place(&old, &dir)
                .with_context(|| format!("cannot move {} to {}", old.display(), dir.display()))?;
        }
        Ok(dir)
    }

    /// Stores a login. When `mounted_in` names the seat it came from, the entry already mounted
    /// there is updated (its tokens are newer); otherwise an entry with the same
    /// grant is updated, or a new one is created. A given `label` must not name another login.
    pub fn put(
        &mut self,
        secret: &Secret,
        oauth_account: Option<Value>,
        account: Option<&config_json::Account>,
        label: Option<&str>,
        mounted_in: Option<&SeatId>,
    ) -> anyhow::Result<String> {
        let choice = match label {
            Some(l) => Label::Given(l),
            None => Label::Default(
                account
                    .and_then(|a| {
                        a.email
                            .as_ref()
                            .map(|e| e.split('@').next().unwrap_or(e).to_string())
                    })
                    .unwrap_or_else(|| "login".into()),
            ),
        };
        self.put_labelled(secret, oauth_account, account, choice, mounted_in)
    }

    fn put_labelled(
        &mut self,
        secret: &Secret,
        oauth_account: Option<Value>,
        account: Option<&config_json::Account>,
        label: Label<'_>,
        mounted_in: Option<&SeatId>,
    ) -> anyhow::Result<String> {
        let meta = secret.meta();
        let now = Utc::now();
        let account_with_meta: Option<config_json::Account> = account.map(|a| {
            let mut a = a.clone();
            a.apply_credentials(&meta);
            a
        });
        let account = account_with_meta.as_ref();
        let account_id = account.map(|a| a.id().to_string());
        let logins = &self.index.logins;
        let existing = mounted_in
            .and_then(|seat| logins.iter().find(|l| l.mounted.as_ref() == Some(seat)))
            .filter(|l| {
                account_id.is_none() || l.account_uuid.is_none() || l.account_uuid == account_id
            })
            .or_else(|| {
                logins
                    .iter()
                    .find(|l| l.meta.fingerprint == meta.fingerprint)
            })
            .map(|l| l.id.clone());
        // An existing login keeps its label unless one is given; a new one gets a free one.
        let new_label = match (&label, &existing) {
            (Label::Given(l), id) => {
                self.index.check_label(l, id.as_deref())?;
                Some(l.to_string())
            }
            (Label::Default(_), Some(_)) => None,
            (Label::Default(base), None) => Some(self.index.unique_label(base)),
        };
        let id = match existing {
            Some(id) => {
                let l = self
                    .index
                    .find_mut(&id)
                    .context("the vault index changed under the lock")?;
                l.meta = meta;
                l.updated_at = now;
                if oauth_account.is_some() {
                    l.oauth_account = oauth_account;
                }
                if let Some(a) = account {
                    l.account_uuid = Some(a.id().to_string());
                    l.email = a.email.clone().or(l.email.take());
                    l.plan = a.effective_tier().or(l.plan.take());
                }
                if let Some(lb) = new_label {
                    l.label = lb;
                }
                if mounted_in.is_some() {
                    l.mounted = mounted_in.cloned();
                    l.mounted_label = None;
                }
                id
            }
            None => {
                let id = format!("l{}", meta.fingerprint.chars().take(10).collect::<String>());
                self.index.logins.push(Login {
                    id: id.clone(),
                    label: new_label.context("a new login without a label")?,
                    account_uuid: account_id,
                    email: account.and_then(|a| a.email.clone()),
                    plan: account.and_then(|a| a.effective_tier()),
                    meta,
                    oauth_account,
                    captured_at: now,
                    updated_at: now,
                    mounted: mounted_in.cloned(),
                    mounted_label: None,
                });
                id
            }
        };
        self.set_secret(&id, secret);
        Ok(id)
    }

    /// Takes the seat's current login into the vault.
    pub fn capture(&mut self, seat: &Seat, label: Option<&str>) -> anyhow::Result<Option<String>> {
        self.sync(std::slice::from_ref(seat))?;
        let s = read_seat(seat)?;
        let Some(secret) = s.secret.clone() else {
            return Ok(None);
        };
        let id = self.save_seat_login(seat, &s, &secret, label)?;
        self.journal(&format!(
            "capture seat={} login={}",
            seat.label,
            self.index.label_of(&id).as_deref().unwrap_or("?")
        ));
        Ok(Some(id))
    }

    /// Saves the login a seat holds; a new one is named after the seat unless a label is given.
    /// When the account behind it is not confirmed yet, it is saved without one, never under the
    /// stale profile's name.
    fn save_seat_login(
        &mut self,
        seat: &Seat,
        s: &SeatLogin,
        secret: &Secret,
        label: Option<&str>,
    ) -> anyhow::Result<String> {
        let state = self.observe(seat, s);
        let choice = match label {
            Some(l) => Label::Given(l),
            None => Label::Default(seat.label.clone()),
        };
        if state.unverified_since.is_some() {
            return self.put_labelled(secret, None, None, choice, Some(&seat.id));
        }
        self.put_labelled(
            secret,
            s.oauth_account.clone(),
            s.account.as_ref(),
            choice,
            Some(&seat.id),
        )
    }

    pub fn rename(&mut self, key: &str, new_label: &str) -> anyhow::Result<()> {
        let id = self.find(key).map(|l| l.id.clone())?;
        self.index.check_label(new_label, Some(&id))?;
        if let Some(l) = self.index.find_mut(&id) {
            l.label = new_label.to_string();
        }
        Ok(())
    }

    /// Forgets a login. Its secret leaves the disk only after the index no longer lists it.
    pub fn remove(&mut self, key: &str, force: bool) -> anyhow::Result<Login> {
        let l = self.find(key).cloned()?;
        let in_seat = l
            .mounted
            .as_ref()
            .map(|m| self.index.seat_name(m))
            .or_else(|| l.mounted_label.clone());
        if let Some(seat) = in_seat
            && !force
        {
            return Err(VaultError::InUse {
                login: l.label,
                seat,
            }
            .into());
        }
        self.index.logins.retain(|x| x.id != l.id);
        self.secrets.remove(&l.id);
        self.journal(&format!("remove login={}", l.label));
        Ok(l)
    }
}

/// How a stored login is named.
enum Label<'a> {
    /// Chosen by the user: refused when another login has it.
    Given(&'a str),
    /// A base for a new login's label (made unique); an existing login keeps its own.
    Default(String),
}

#[cfg(test)]
mod tests;
