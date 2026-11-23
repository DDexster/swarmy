//! Identities: a seat, a session, an account. Each is a zero-cost newtype over the text Claude
//! Code or swarmy stores, so a seat id cannot be passed where a session id or a display label is
//! expected (reviews H3, H7, M10).

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The identity of a seat (a Claude Code config dir): the first 16 hex digits of the SHA-256 of
/// its canonical path. It is what the index, the vault, the status-line files and the fleet
/// summary store; a seat's label is for display only, so renaming a seat (a `label` in
/// `config.toml`, another dir of the same name found first) changes no attribution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct SeatId(String);

impl SeatId {
    /// Hex digits kept of the digest.
    pub const LEN: usize = 16;

    /// The id of a config dir from its canonical path (the caller canonicalizes: `core` does no
    /// I/O). The status line names its reading files the same way.
    pub fn of_canonical_dir(dir: &Path) -> SeatId {
        let h = Sha256::digest(dir.to_string_lossy().as_bytes());
        let mut s = hex::encode(h);
        s.truncate(Self::LEN);
        SeatId(s)
    }

    /// An id read back from where swarmy stored it (the index, the vault, the fleet summary).
    /// Older stores held a seat's label here instead; [`SeatId::looks_like_id`] tells them apart.
    pub fn from_stored(s: impl Into<String>) -> SeatId {
        SeatId(s.into())
    }

    /// Whether `s` has the shape of a seat id (16 lowercase hex digits), as opposed to a label
    /// stored by an older swarmy.
    pub fn looks_like_id(s: &str) -> bool {
        s.len() == Self::LEN
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Tests name seats with short made-up ids.
#[cfg(test)]
impl From<&str> for SeatId {
    fn from(s: &str) -> SeatId {
        SeatId(s.to_string())
    }
}

// Same `Eq`, `Ord` and `Hash` as the text: maps keyed by id can be read with a `&str`.
impl std::borrow::Borrow<str> for SeatId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SeatId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A Claude Code session id (a uuid in practice, but read from files that may hold anything).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(s: impl Into<String>) -> SessionId {
        SessionId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The first 8 characters (fewer when the id is shorter), for display. Never panics, also
    /// on an empty id or one with multi-byte characters.
    pub fn short(&self) -> &str {
        short(&self.0)
    }
}

impl std::borrow::Borrow<str> for SessionId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> SessionId {
        SessionId(s.to_string())
    }
}

impl PartialEq<str> for SessionId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for SessionId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// An Anthropic account: the `accountUuid` of an OAuth profile. Made by the readers of Claude
/// Code's files and by resolving what a user typed (`--account` takes a uuid, an email or a
/// saved login's label, resolved once at the edge: `app::Ctx::resolve_account`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct AccountId(String);

impl AccountId {
    pub fn new(s: impl Into<String>) -> AccountId {
        AccountId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first 8 characters, for a name of last resort (`acct-1a2b3c4d`).
    pub fn short(&self) -> &str {
        short(&self.0)
    }
}

#[cfg(test)]
impl From<&str> for AccountId {
    fn from(s: &str) -> AccountId {
        AccountId(s.to_string())
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The first 8 characters of `s` (by character, so a multi-byte one is never cut).
pub fn short(s: &str) -> &str {
    match s.char_indices().nth(8) {
        Some((i, _)) => s.get(..i).unwrap_or(s),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids_never_panic() {
        assert_eq!(SessionId::new("").short(), "");
        assert_eq!(SessionId::new("s1").short(), "s1");
        assert_eq!(SessionId::new("12345678").short(), "12345678");
        assert_eq!(
            SessionId::new("0f9e1a2b-3c4d-4e5f-8a9b-0c1d2e3f4a5b").short(),
            "0f9e1a2b"
        );
        // Multi-byte characters are kept whole: byte slicing at 8 would split "é".
        assert_eq!(SessionId::new("abcdefgé-xyz").short(), "abcdefgé");
        assert_eq!(SessionId::new("ééééééééé").short(), "éééééééé");
        assert_eq!(AccountId::new("ab").short(), "ab");
    }

    #[test]
    fn seat_ids_come_from_the_path_only() {
        let a = SeatId::of_canonical_dir(Path::new("/h/.claude-work"));
        assert_eq!(a, SeatId::of_canonical_dir(Path::new("/h/.claude-work")));
        assert_ne!(a, SeatId::of_canonical_dir(Path::new("/h/.claude-home")));
        assert_eq!(a.as_str().len(), SeatId::LEN);
        assert!(SeatId::looks_like_id(a.as_str()));
        assert!(!SeatId::looks_like_id("work"));
        assert!(!SeatId::looks_like_id("0123456789ABCDEF"));
        // The same digest the status line has always named its files with.
        let full = hex::encode(Sha256::digest(b"/h/.claude-work"));
        assert_eq!(a.as_str(), &full[..16]);
    }
}
