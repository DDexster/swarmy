//! `vault/logins.json`: the saved logins and what was last seen in each seat, its format
//! versions, and the rules on labels.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Login, store};
use crate::claude::seats::{Seat, SeatId};
use crate::errors::VaultError;

/// The format of `logins.json`: 2 names seats by id (`core::ids::SeatId`); 1 (no `v`) by label.
const INDEX_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Index {
    #[serde(default = "version_1")]
    v: u32,
    pub(super) logins: Vec<Login>,
    /// What was last seen in each seat, to tell a token refresh from a new login.
    #[serde(default)]
    pub(super) seats: std::collections::BTreeMap<SeatId, SeatSeen>,
    /// Entries of `seats` an older swarmy keyed by a seat label, not matched to a seat yet.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    legacy_seats: std::collections::BTreeMap<String, SeatSeen>,
}

fn version_1() -> u32 {
    1
}

impl Default for Index {
    fn default() -> Self {
        Index {
            v: INDEX_VERSION,
            logins: Vec::new(),
            seats: Default::default(),
            legacy_seats: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct SeatSeen {
    /// The seat's label when it was last seen, for messages only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) label: String,
    pub(super) fingerprint: String,
    pub(super) account_uuid: Option<String>,
    pub(super) profile_at: Option<DateTime<Utc>>,
    /// The refresh token changed and Claude Code has not re-read the profile since.
    pub(super) unverified_since: Option<DateTime<Utc>>,
}

pub(super) fn read_index(dir: &Path) -> anyhow::Result<(Index, Option<String>)> {
    let text = store::read_optional(&dir.join("logins.json"))?;
    let index = match &text {
        Some(t) => {
            let mut i: Index = serde_json::from_str(t)
                .map_err(|e| anyhow::Error::new(e).context(VaultError::Damaged("logins.json")))?;
            i.upgrade()?;
            i
        }
        None => Index::default(),
    };
    Ok((index, text))
}

impl Index {
    /// Reads an older format in place (nothing is lost): version 1 stored seat labels where
    /// version 2 stores seat ids, so they wait in `mounted_label` and `legacy_seats` until
    /// [`Index::adopt_seats`] matches them to a seat.
    pub(super) fn upgrade(&mut self) -> anyhow::Result<()> {
        if self.v > INDEX_VERSION {
            return Err(VaultError::NewerFormat(self.v).into());
        }
        if self.v < 2 {
            for l in &mut self.logins {
                if let Some(label) = l.mounted.take() {
                    l.mounted_label = Some(label.as_str().to_string());
                }
            }
            for (label, mut seen) in std::mem::take(&mut self.seats) {
                seen.label = label.as_str().to_string();
                self.legacy_seats.insert(label.as_str().to_string(), seen);
            }
            self.v = INDEX_VERSION;
        }
        Ok(())
    }

    /// Matches what an older swarmy stored under a seat label to the seat that carries that
    /// label now. A label no current seat carries keeps waiting.
    pub(super) fn adopt_seats(&mut self, seats: &[Seat]) {
        for seat in seats {
            if let Some(mut seen) = self.legacy_seats.remove(&seat.label) {
                seen.label = seat.label.clone();
                self.seats.entry(seat.id.clone()).or_insert(seen);
            }
            for l in &mut self.logins {
                if l.mounted.is_none() && l.mounted_label.as_deref() == Some(seat.label.as_str()) {
                    l.mounted = Some(seat.id.clone());
                    l.mounted_label = None;
                }
            }
        }
    }

    /// How messages name a seat id: its last label, else the id.
    pub(super) fn seat_name(&self, id: &SeatId) -> String {
        self.seats
            .get(id)
            .map(|s| s.label.clone())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| id.to_string())
    }

    /// The login `key` names: its label, its id or its email. A key that names two logins
    /// (one's label is another's email, two logins of one email) is an error that lists them,
    /// never a guess (review L3).
    pub(super) fn find(&self, key: &str) -> Result<&Login, VaultError> {
        let hits: Vec<&Login> = self
            .logins
            .iter()
            .filter(|l| l.label == key || l.id == key || l.email.as_deref() == Some(key))
            .collect();
        match hits.as_slice() {
            [one] => Ok(one),
            [] => Err(VaultError::NoLogin(key.to_string())),
            many => Err(VaultError::AmbiguousLogin {
                key: key.to_string(),
                logins: many.iter().map(|l| l.label.clone()).collect(),
            }),
        }
    }

    pub(super) fn find_mut(&mut self, id: &str) -> Option<&mut Login> {
        self.logins.iter_mut().find(|l| l.id == id)
    }

    pub(super) fn label_of(&self, id: &str) -> Option<String> {
        self.logins
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.label.clone())
    }

    /// The one place that decides whether `label` may name the login `id` (`None`: a new one).
    pub(super) fn check_label(&self, label: &str, id: Option<&str>) -> anyhow::Result<()> {
        if label.trim().is_empty() {
            return Err(VaultError::LabelEmpty.into());
        }
        if self
            .logins
            .iter()
            .any(|l| l.label == label && Some(l.id.as_str()) != id)
        {
            return Err(VaultError::LabelTaken(label.to_string()).into());
        }
        Ok(())
    }

    /// `base`, or `base-2`, `base-3`… whichever no login has.
    pub(super) fn unique_label(&self, base: &str) -> String {
        let mut label = base.to_string();
        let mut n = 2;
        while self.logins.iter().any(|l| l.label == label) {
            label = format!("{base}-{n}");
            n += 1;
        }
        label
    }

    /// Repairs labels duplicated by older versions: later duplicates get a unique suffix.
    pub(super) fn dedupe_labels(&mut self) -> Vec<(String, String)> {
        let mut renamed = Vec::new();
        for i in 0..self.logins.len() {
            let label = self.logins[i].label.clone();
            if self.logins[..i].iter().any(|l| l.label == label) {
                let new = self.unique_label(&label);
                self.logins[i].label = new.clone();
                renamed.push((label, new));
            }
        }
        renamed
    }
}
