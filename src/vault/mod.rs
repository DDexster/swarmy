//! The login vault: saved Claude Code OAuth logins, and switching which one a seat uses.
//!
//! A *login* is the `claudeAiOauth` object of a credentials file (an OAuth grant) plus facts
//! about it. A *seat* is a Claude Code config dir. Claude Code rotates the refresh token when it
//! refreshes, so a copy goes stale while the login is in use: the vault tracks which seat each
//! login is mounted in and, before every change, takes the seat's current tokens back first.
//!
//! Secrets live in `vault/secrets.json` with mode 0600 — the same protection Claude Code gives the
//! very same tokens in `.credentials.json` on Linux and Windows.

pub mod store;

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude::config_json;
use crate::claude::credentials::{self, LoginMeta, Secret};
use crate::claude::seats::Seat;
use store::{SecretStore, atomic_write};

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
    pub mounted: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    logins: Vec<Login>,
    /// What was last seen in each seat, to tell a token refresh from a new login.
    #[serde(default)]
    seats: std::collections::BTreeMap<String, SeatSeen>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct SeatSeen {
    fingerprint: String,
    account_uuid: Option<String>,
    profile_at: Option<DateTime<Utc>>,
    /// The refresh token changed and Claude Code has not re-read the profile since.
    unverified_since: Option<DateTime<Utc>>,
}

/// What a seat holds, as the vault sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeatState {
    pub fingerprint: Option<String>,
    /// The account from Claude Code's profile, when it is known to belong to the current login.
    pub account_uuid: Option<String>,
    /// The login changed after the profile was read: the account behind it is not known yet.
    pub unverified_since: Option<DateTime<Utc>>,
    /// The saved login that holds exactly these tokens.
    pub saved_as: Option<String>,
}

/// Refresh tokens are issued for 30 days: a token issued after the profile was read means a new
/// login the profile does not describe yet.
const REFRESH_LIFETIME_DAYS: i64 = 30;

pub struct Vault {
    dir: PathBuf,
    index: Index,
    pub store: Box<dyn SecretStore>,
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
                bail!(
                    "on macOS Claude Code keeps the login in the Keychain; the vault cannot read it yet"
                );
            }
            None
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("cannot read {}", seat.credentials_file().display()));
        }
    };
    let cj = config_json::read(&seat.claude_json).unwrap_or_default();
    let written_at = std::fs::metadata(seat.credentials_file())
        .ok()
        .and_then(|m| m.modified().ok())
        .map(DateTime::<Utc>::from);
    Ok(SeatLogin {
        secret,
        written_at,
        oauth_account: cj.oauth_account_raw,
        account: cj.account,
    })
}

impl Vault {
    pub fn open(dir: &Path) -> anyhow::Result<Vault> {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        store::restrict_dir(dir)?;
        let index = match std::fs::read_to_string(dir.join("logins.json")) {
            Ok(t) => serde_json::from_str(&t).context("vault/logins.json is damaged")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Index::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Vault {
            dir: dir.to_path_buf(),
            index,
            store: Box::new(store::FileStore::new(dir.join("secrets.json"))),
        })
    }

    fn save(&self) -> anyhow::Result<()> {
        atomic_write(
            &self.dir.join("logins.json"),
            serde_json::to_string_pretty(&self.index)?.as_bytes(),
        )
    }

    pub fn logins(&self) -> &[Login] {
        &self.index.logins
    }

    pub fn find(&self, key: &str) -> Option<&Login> {
        self.index
            .logins
            .iter()
            .find(|l| l.label == key || l.id == key || l.email.as_deref() == Some(key))
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Login> {
        self.index.logins.iter_mut().find(|l| l.id == id)
    }

    fn unique_label(&self, base: &str) -> String {
        let mut label = base.to_string();
        let mut n = 2;
        while self.index.logins.iter().any(|l| l.label == label) {
            label = format!("{base}-{n}");
            n += 1;
        }
        label
    }

    fn journal(&self, line: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("journal.log"))
        {
            let _ = writeln!(f, "{} {line}", Utc::now().to_rfc3339());
        }
    }

    /// Stores a login. When `mounted_in` names the seat it came from, the entry already mounted
    /// there is updated (its refresh token may have rotated); otherwise an entry with the same
    /// grant is updated, or a new one is created.
    pub fn put(
        &mut self,
        secret: &Secret,
        oauth_account: Option<Value>,
        account: Option<&config_json::Account>,
        label: Option<&str>,
        mounted_in: Option<&str>,
    ) -> anyhow::Result<String> {
        let meta = secret.meta();
        let now = Utc::now();
        let existing = mounted_in
            .and_then(|seat| {
                self.index
                    .logins
                    .iter()
                    .find(|l| l.mounted.as_deref() == Some(seat))
            })
            .filter(|l| {
                account.is_none()
                    || l.account_uuid.is_none()
                    || l.account_uuid.as_deref() == account.map(|a| a.account_uuid.as_str())
            })
            .or_else(|| {
                self.index
                    .logins
                    .iter()
                    .find(|l| l.meta.fingerprint == meta.fingerprint)
            })
            .map(|l| l.id.clone());
        let id = match existing {
            Some(id) => {
                let l = self.find_mut(&id).unwrap();
                l.meta = meta;
                l.updated_at = now;
                if oauth_account.is_some() {
                    l.oauth_account = oauth_account;
                }
                if let Some(a) = account {
                    l.account_uuid = Some(a.account_uuid.clone());
                    l.email = a.email.clone().or(l.email.take());
                    l.plan = a.rate_limit_tier.clone().or(l.plan.take());
                }
                if let Some(lb) = label {
                    l.label = lb.to_string();
                }
                if mounted_in.is_some() {
                    l.mounted = mounted_in.map(str::to_string);
                }
                id
            }
            None => {
                let base = label
                    .map(str::to_string)
                    .or_else(|| {
                        account.and_then(|a| {
                            a.email
                                .as_ref()
                                .map(|e| e.split('@').next().unwrap_or(e).to_string())
                        })
                    })
                    .unwrap_or_else(|| "login".into());
                let label = if label.is_some() {
                    base
                } else {
                    self.unique_label(&base)
                };
                if self.index.logins.iter().any(|l| l.label == label) {
                    bail!("a login labelled '{label}' already exists");
                }
                let id = format!("l{}", &meta.fingerprint[..10.min(meta.fingerprint.len())]);
                self.index.logins.push(Login {
                    id: id.clone(),
                    label,
                    account_uuid: account.map(|a| a.account_uuid.clone()),
                    email: account.and_then(|a| a.email.clone()),
                    plan: account.and_then(|a| a.rate_limit_tier.clone()),
                    meta,
                    oauth_account,
                    captured_at: now,
                    updated_at: now,
                    mounted: mounted_in.map(str::to_string),
                });
                id
            }
        };
        self.store.put(&id, secret)?;
        self.save()?;
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
            self.find(&id).map(|l| l.label.as_str()).unwrap_or("?")
        ));
        Ok(Some(id))
    }

    /// Saves the login a seat holds. When the account behind it is not confirmed yet, it is saved
    /// without one (labelled after the seat), never under the stale profile's name.
    fn save_seat_login(
        &mut self,
        seat: &Seat,
        s: &SeatLogin,
        secret: &Secret,
        label: Option<&str>,
    ) -> anyhow::Result<String> {
        let state = self.observe(seat, s);
        if state.unverified_since.is_some() {
            let base = label
                .map(str::to_string)
                .unwrap_or_else(|| seat.label.clone());
            let label = if label.is_some() {
                base
            } else {
                self.unique_label(&base)
            };
            return self.put(secret, None, None, Some(&label), Some(&seat.label));
        }
        // Not in the vault yet and no label given: name it after the seat.
        let label =
            match label {
                Some(l) => Some(l.to_string()),
                None if !self.index.logins.iter().any(|l| {
                    l.meta.fingerprint == state.fingerprint.clone().unwrap_or_default()
                }) =>
                {
                    Some(self.unique_label(&seat.label))
                }
                None => None,
            };
        self.put(
            secret,
            s.oauth_account.clone(),
            s.account.as_ref(),
            label.as_deref(),
            Some(&seat.label),
        )
    }

    /// Compares a seat with what was seen there before. A new refresh token with no newer
    /// profile means a new login (or a restored backup) whose account is not known yet.
    fn observe(&mut self, seat: &Seat, s: &SeatLogin) -> SeatState {
        let Some(sec) = &s.secret else {
            self.index.seats.remove(&seat.label);
            return SeatState::default();
        };
        let meta = sec.meta();
        let fp = meta.fingerprint.clone();
        let profile_at = s.account.as_ref().and_then(|a| a.profile_fetched_at);
        let account = s.account.as_ref().map(|a| a.account_uuid.clone());
        let prev = self.index.seats.get(&seat.label).cloned();
        let unverified_since = match &prev {
            // First sight: a refresh token issued after the profile was read is a newer login;
            // a file written after the profile with an access token already expired at that time
            // was copied in (Claude Code only writes fresh tokens).
            None => {
                let issued = meta
                    .refresh_expires_at
                    .map(|t| t - chrono::Duration::days(REFRESH_LIFETIME_DAYS));
                let copied_in = match (s.written_at, meta.access_expires_at, profile_at) {
                    (Some(w), Some(exp), Some(p))
                        if w > p && exp + chrono::Duration::minutes(5) < w =>
                    {
                        Some(w)
                    }
                    _ => None,
                };
                match (issued, profile_at) {
                    (Some(i), Some(p)) if i > p + chrono::Duration::minutes(10) => Some(i),
                    _ => copied_in,
                }
            }
            Some(p) if p.fingerprint != fp => {
                if profile_at > p.profile_at {
                    None
                } else {
                    Some(Utc::now())
                }
            }
            Some(p) => p.unverified_since.filter(|_| profile_at <= p.profile_at),
        };
        let seen = SeatSeen {
            fingerprint: fp.clone(),
            account_uuid: account.clone(),
            profile_at: if unverified_since.is_some() {
                prev.as_ref().and_then(|p| p.profile_at).or(profile_at)
            } else {
                profile_at
            },
            unverified_since,
        };
        if prev.as_ref() != Some(&seen) {
            self.index.seats.insert(seat.label.clone(), seen);
            let _ = self.save();
        }
        let saved_as = self
            .index
            .logins
            .iter()
            .find(|l| l.meta.fingerprint == fp)
            .map(|l| l.label.clone());
        SeatState {
            fingerprint: Some(fp),
            account_uuid: if unverified_since.is_some() {
                None
            } else {
                account
            },
            unverified_since,
            saved_as,
        }
    }

    /// The state of every seat (also brings the vault up to date, see `sync`).
    pub fn seat_states(
        &mut self,
        seats: &[Seat],
    ) -> anyhow::Result<std::collections::BTreeMap<String, SeatState>> {
        self.sync(seats)?;
        let mut out = std::collections::BTreeMap::new();
        for seat in seats {
            if let Ok(s) = read_seat(seat) {
                out.insert(seat.label.clone(), self.observe(seat, &s));
            }
        }
        Ok(out)
    }

    /// Keeps the vault copy of every mounted login current with its seat.
    ///
    /// Claude Code refreshes the access token every few hours and keeps the refresh token: the
    /// saved copy simply takes the new access token. A different refresh token is another login
    /// (a `/login`, a restored backup): the saved login is left untouched and no longer counts as
    /// mounted there, so its tokens are never overwritten by someone else's.
    pub fn sync(&mut self, seats: &[Seat]) -> anyhow::Result<()> {
        for seat in seats {
            let Some(mounted) = self
                .index
                .logins
                .iter()
                .find(|l| l.mounted.as_deref() == Some(&seat.label))
                .cloned()
            else {
                continue;
            };
            let Ok(s) = read_seat(seat) else { continue };
            match s.secret {
                Some(sec) if sec.meta().fingerprint == mounted.meta.fingerprint => {
                    // Same grant, maybe a fresher access token: keep the copy current.
                    if self.store.get(&mounted.id)?.as_ref() != Some(&sec) {
                        self.store.put(&mounted.id, &sec)?;
                        if let Some(l) = self.find_mut(&mounted.id) {
                            l.meta = sec.meta();
                            l.updated_at = Utc::now();
                        }
                        self.save()?;
                    }
                }
                Some(_) => {
                    // Another login now lives in that seat; ours keeps its own tokens.
                    if let Some(l) = self.find_mut(&mounted.id) {
                        l.mounted = None;
                        self.save()?;
                    }
                    self.journal(&format!(
                        "seat={} has a new login; '{}' kept unchanged",
                        seat.label, mounted.label
                    ));
                }
                None => {
                    if let Some(l) = self.find_mut(&mounted.id) {
                        l.mounted = None;
                        self.save()?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Writes `login_key` into `seat`, first saving whatever the seat holds.
    pub fn switch(
        &mut self,
        seat: &Seat,
        login_key: &str,
        allow_shared: bool,
    ) -> anyhow::Result<SwitchReport> {
        let _lock = store::Lock::acquire(&self.dir.join("lock"))?;
        let target = self.find(login_key).cloned().with_context(|| {
            format!("no login '{login_key}' in the vault (see `agentmana accounts`)")
        })?;
        if let Some(other) = target.mounted.as_deref().filter(|m| *m != seat.label)
            && !allow_shared
        {
            bail!(
                "login '{}' is in use in seat '{other}'. One OAuth grant in two seats breaks one of them when the token rotates. \
                 Log in separately there (`agentmana accounts add`), or pass --share to copy it anyway.",
                target.label
            );
        }
        let secret = self.store.get(&target.id)?.with_context(|| {
            format!("the secret of '{}' is missing from the vault", target.label)
        })?;

        // 1. Save what the seat holds now.
        self.sync(std::slice::from_ref(seat))?;
        let before = read_seat(seat)?;
        let known_before = before.secret.as_ref().is_some_and(|sec| {
            self.index
                .logins
                .iter()
                .any(|l| l.meta.fingerprint == sec.meta().fingerprint)
        });
        let previous = match &before.secret {
            Some(sec) => Some(self.save_seat_login(seat, &before, sec, None)?),
            None => None,
        };
        let auto_saved = previous
            .as_ref()
            .filter(|_| !known_before)
            .and_then(|id| self.index.logins.iter().find(|l| l.id == *id))
            .map(|l| l.label.clone());
        if previous.as_deref() == Some(target.id.as_str()) {
            return Ok(SwitchReport {
                auto_saved: None,
                seat: seat.label.clone(),
                from: Some(target.label.clone()),
                to: target.label,
                backup: None,
                unchanged: true,
            });
        }

        // 2. Back up both files.
        let stamp = Utc::now().format("%Y%m%dT%H%M%S");
        let bdir = self.dir.join("backups");
        std::fs::create_dir_all(&bdir)?;
        let cred_text = std::fs::read_to_string(seat.credentials_file()).ok();
        let cj_text = std::fs::read_to_string(&seat.claude_json).ok();
        if let Some(t) = &cred_text {
            atomic_write(
                &bdir.join(format!("{stamp}-{}-credentials.json", seat.label)),
                t.as_bytes(),
            )?;
        }
        if let Some(t) = &cj_text {
            atomic_write(
                &bdir.join(format!("{stamp}-{}-claude.json", seat.label)),
                t.as_bytes(),
            )?;
        }
        store::prune_backups(&bdir, 40)?;

        // 3. Write the new login, keeping mcpOAuth and every other key.
        let new_creds = credentials::with_login(cred_text.as_deref(), &secret)?;
        atomic_write(&seat.credentials_file(), new_creds.as_bytes())?;
        // 4. The account shown by Claude Code.
        if let (Some(t), Some(acct)) = (&cj_text, &target.oauth_account) {
            atomic_write(
                &seat.claude_json,
                config_json::with_oauth_account(t, Some(acct))?.as_bytes(),
            )?;
        }

        // 5. Bookkeeping.
        let from_label = previous
            .as_ref()
            .and_then(|id| self.index.logins.iter().find(|l| l.id == *id))
            .map(|l| l.label.clone());
        for l in &mut self.index.logins {
            if l.mounted.as_deref() == Some(&seat.label) {
                l.mounted = None;
            }
        }
        if let Some(l) = self.find_mut(&target.id) {
            l.mounted = Some(seat.label.clone());
        }
        self.save()?;
        self.journal(&format!(
            "switch seat={} from={} to={}",
            seat.label,
            from_label.as_deref().unwrap_or("-"),
            target.label
        ));
        Ok(SwitchReport {
            auto_saved,
            seat: seat.label.clone(),
            from: from_label,
            to: target.label,
            backup: Some(bdir),
            unchanged: false,
        })
    }

    pub fn rename(&mut self, key: &str, new_label: &str) -> anyhow::Result<()> {
        if self.index.logins.iter().any(|l| l.label == new_label) {
            bail!("a login labelled '{new_label}' already exists");
        }
        let id = self
            .find(key)
            .map(|l| l.id.clone())
            .with_context(|| format!("no login '{key}'"))?;
        self.find_mut(&id).unwrap().label = new_label.to_string();
        self.save()
    }

    pub fn remove(&mut self, key: &str, force: bool) -> anyhow::Result<Login> {
        let l = self
            .find(key)
            .cloned()
            .with_context(|| format!("no login '{key}'"))?;
        if let Some(seat) = &l.mounted
            && !force
        {
            bail!(
                "login '{}' is in use in seat '{seat}'; pass --force to forget it anyway (the seat keeps working)",
                l.label
            );
        }
        self.store.delete(&l.id)?;
        self.index.logins.retain(|x| x.id != l.id);
        self.save()?;
        self.journal(&format!("remove login={}", l.label));
        Ok(l)
    }
}

#[derive(Debug)]
pub struct SwitchReport {
    /// The seat held a login that was not in the vault; it was saved under this label.
    pub auto_saved: Option<String>,
    pub seat: String,
    pub from: Option<String>,
    pub to: String,
    pub backup: Option<PathBuf>,
    pub unchanged: bool,
}

#[cfg(test)]
mod tests;
