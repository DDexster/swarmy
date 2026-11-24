//! Switching which saved login a seat uses: the vault's one write into a harness's config dir
//! (Claude Code: `.credentials.json` plus `oauthAccount` in `.claude.json`; Antigravity: its
//! `antigravity-oauth-token`).

use std::path::PathBuf;

use super::{Login, SEAT_BACKUPS, VaultError, VaultTx, read_seat, store};
use crate::claude::config_json;
use crate::claude::credentials;
use crate::claude::seats::{Seat, SeatId};
use crate::core::harness::Harness;

/// How many times a switch starts again because Claude Code rewrote the seat's credentials
/// between swarmy saving them and replacing them.
const SWITCH_ATTEMPTS: usize = 5;

impl VaultTx<'_> {
    /// Writes `login_key` into `seat`, first saving whatever the seat holds. The login may be in
    /// other seats too and stays there: one grant can serve several seats (both refresh the
    /// access token; Claude Code and agy keep the refresh token).
    ///
    /// All or nothing for the seat: the seat's login is saved first (on disk before anything in
    /// the seat changes), both seat files are backed up, then `.credentials.json` is replaced
    /// only while it still holds what was saved (a token Claude Code refreshed meanwhile is saved
    /// again first, never overwritten), then `oauthAccount` is merged into `.claude.json`. When
    /// that last write fails, the credentials go back as they were; if even that is not possible
    /// (Claude Code wrote them since), the vault records the login the seat holds now, so the
    /// vault and the seat never disagree about which grant is where (and [`VaultTx::sync`]
    /// repairs the same after a crash between the two writes).
    pub fn switch(&mut self, seat: &Seat, login_key: &str) -> anyhow::Result<SwitchReport> {
        // A label names one login per harness: the seat's first, else whatever it names (a
        // login of another harness is refused below, by name).
        let target = match self.index.find_in(login_key, Some(seat.harness)) {
            Ok(l) => l.clone(),
            Err(crate::errors::VaultError::NoLogin(_)) => self.find(login_key).cloned()?,
            Err(e) => return Err(e.into()),
        };
        if target.harness != seat.harness {
            return Err(VaultError::WrongHarness {
                login: target.label,
                login_harness: target.harness.display_name().into(),
                seat: seat.label.clone(),
                seat_harness: seat.harness.display_name().into(),
            }
            .into());
        }
        let shared_with: Vec<String> = target
            .mounted
            .iter()
            .filter(|m| **m != seat.id)
            .map(|m| self.index.seat_name(m))
            .collect();
        let secret = self
            .secret(&target.id)
            .ok_or_else(|| VaultError::MissingSecret(target.label.clone()))?;
        let mut auto_saved: Option<String> = None;
        for _ in 0..SWITCH_ATTEMPTS {
            // 1. Save what the seat holds now, on disk before the seat is overwritten.
            self.sync(std::slice::from_ref(seat))?;
            let cred_text = store::read_optional(&seat.credentials_file())?;
            let cj_text = match seat.harness {
                Harness::Claude => store::read_optional(&seat.claude_json)?,
                Harness::Agy => None,
            };
            let before = read_seat(seat)?;
            if store::read_optional(&seat.credentials_file())? != cred_text {
                // Claude Code refreshed the token while it was read: read it again.
                continue;
            }
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
            if auto_saved.is_none() && !known_before {
                auto_saved = previous.as_ref().and_then(|id| self.index.label_of(id));
            }
            if previous.as_deref() == Some(target.id.as_str()) {
                return Ok(SwitchReport {
                    auto_saved: None,
                    seat: seat.label.clone(),
                    from: Some(target.label.clone()),
                    to: target.label,
                    backup: None,
                    unchanged: true,
                    shared_with,
                });
            }
            self.save()?;

            // 2. Back up both files, kept per seat.
            let bdir = self.seat_backups(seat)?;
            match seat.harness {
                Harness::Claude => store::backup(
                    &bdir,
                    &[
                        ("credentials.json", cred_text.as_deref()),
                        ("claude.json", cj_text.as_deref()),
                    ],
                    SEAT_BACKUPS,
                )?,
                Harness::Agy => store::backup(
                    &bdir,
                    &[(crate::agy::credentials::TOKEN_FILE, cred_text.as_deref())],
                    SEAT_BACKUPS,
                )?,
            }

            #[cfg(test)]
            super::tests::hooks::before_seat_write();
            // 3. Write the new login, keeping mcpOAuth and every other key, only over the very
            //    text whose login step 1 saved.
            let new_creds = match seat.harness {
                Harness::Claude => credentials::with_login(cred_text.as_deref(), &secret)?,
                Harness::Agy => crate::agy::credentials::with_login(&secret)?,
            };
            if !store::replace_if_unchanged(
                &seat.credentials_file(),
                cred_text.as_deref(),
                Some(new_creds.as_bytes()),
            )? {
                continue;
            }
            // 4. The account shown by Claude Code: only `oauthAccount` changes, merged into the
            //    file as it is right now (Claude Code rewrites it every few seconds).
            if seat.harness == Harness::Claude
                && let Some(acct) = &target.oauth_account
                && let Err(e) = store::rewrite_merging(&seat.claude_json, |t| {
                    config_json::with_oauth_account(t, Some(acct))
                })
            {
                return Err(self.undo_credentials(
                    seat,
                    &target,
                    cred_text.as_deref(),
                    &new_creds,
                    e,
                ));
            }

            // 5. Bookkeeping.
            let from_label = previous.as_ref().and_then(|id| self.index.label_of(id));
            self.mount(&target.id, &seat.id);
            let target_profile_at = target
                .oauth_account
                .as_ref()
                .and_then(config_json::Account::from_value)
                .and_then(|a| a.profile_fetched_at);
            // Without a saved profile, `.claude.json` still names the previous account: the
            // seat's account is unknown until Claude Code reads the new login's profile.
            let now = chrono::Utc::now();
            // An agy token file names its own account.
            let known_profile = target.account_uuid.is_some()
                && (seat.harness == Harness::Agy || target.oauth_account.is_some());
            self.index.seats.insert(
                seat.id.clone(),
                super::logins::SeatSeen {
                    label: seat.label.clone(),
                    fingerprint: target.meta.fingerprint.clone(),
                    account_uuid: target.account_uuid.clone().filter(|_| known_profile),
                    profile_at: target_profile_at,
                    unverified_since: (!known_profile).then_some(now),
                    switched_at: Some(now),
                },
            );
            self.journal(&format!(
                "switch seat={} from={} to={}",
                seat.label,
                from_label.as_deref().unwrap_or("-"),
                target.label
            ));
            return Ok(SwitchReport {
                auto_saved,
                seat: seat.label.clone(),
                from: from_label,
                to: target.label,
                backup: Some(bdir),
                unchanged: false,
                shared_with,
            });
        }
        Err(VaultError::SeatBusy(seat.credentials_file()).into())
    }

    /// Records `id` as the login in `seat` (a seat holds one login; the login may be in other
    /// seats as well).
    pub(super) fn mount(&mut self, id: &str, seat: &SeatId) {
        for l in &mut self.index.logins {
            if l.id != id {
                l.mounted.retain(|m| m != seat);
            }
        }
        if let Some(l) = self.index.find_mut(id)
            && !l.is_in(seat)
        {
            l.mounted.push(seat.clone());
        }
    }

    /// A switch whose `.claude.json` write failed (`err`) after the credentials were written:
    /// puts the credentials back as they were, unless Claude Code has written them since. Then
    /// the seat really runs the new login, and the vault says so (saved right away: the change
    /// itself fails, so `edit` would drop it).
    fn undo_credentials(
        &mut self,
        seat: &Seat,
        target: &Login,
        before: Option<&str>,
        written: &str,
        err: anyhow::Error,
    ) -> anyhow::Error {
        let restored = store::replace_if_unchanged(
            &seat.credentials_file(),
            Some(written),
            before.map(str::as_bytes),
        );
        if let Ok(true) = restored {
            self.journal(&format!(
                "switch seat={} to={} failed, seat restored: {err:#}",
                seat.label, target.label
            ));
            return err.context(format!(
                "the switch of seat '{}' failed; the seat is as it was",
                seat.label
            ));
        }
        self.mount(&target.id, &seat.id);
        let saved = self.save();
        self.journal(&format!(
            "switch seat={} to={} half done (credentials written, .claude.json not): {err:#}",
            seat.label, target.label
        ));
        let what = if let Err(e) = restored {
            format!(".claude.json not written ({err:#}), credentials not restored ({e:#})")
        } else if let Err(e) = saved {
            format!(".claude.json not written ({err:#}), vault not saved ({e:#})")
        } else {
            format!(
                ".claude.json not written ({err:#}); Claude Code refreshed the new login meanwhile"
            )
        };
        VaultError::SwitchIncomplete {
            seat: seat.label.clone(),
            what,
        }
        .into()
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
    /// Other seats that hold the same login.
    pub shared_with: Vec<String>,
}
