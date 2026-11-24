//! Agent harnesses (clients/runtimes) swarmy reads from disk.
//!
//! A seat is one config/data directory of a harness (for example `~/.claude`, `~/.claude-work`,
//! `~/.gemini/antigravity-cli`). Model providers (Anthropic, Google, OpenAI) are separate from
//! harnesses: Antigravity and OpenCode can run models from several providers.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An agent harness whose transcripts and live state swarmy can read offline.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    #[default]
    Claude,
    Agy,
}

impl Harness {
    /// Every harness swarmy knows, in display order.
    pub const ALL: [Harness; 2] = [Harness::Claude, Harness::Agy];

    /// Stable key stored in SQLite (`seats.harness`), JSON output and CLI flags.
    pub const TAGS: [&'static str; 2] = ["claude", "agy"];

    pub fn key(self) -> &'static str {
        match self {
            Harness::Claude => Self::TAGS[0],
            Harness::Agy => Self::TAGS[1],
        }
    }

    /// Compact 2–3 character badge for dense TUI rows when more than one harness is present.
    pub fn short(self) -> &'static str {
        match self {
            Harness::Claude => "cc",
            Harness::Agy => "agy",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::Agy => "Antigravity",
        }
    }

    pub fn from_key(s: &str) -> Option<Harness> {
        Harness::ALL
            .into_iter()
            .find(|h| h.key() == s || h.short() == s || h.aliases().contains(&s))
    }

    /// Other names a user may type for this harness.
    fn aliases(self) -> &'static [&'static str] {
        match self {
            Harness::Claude => &["claude-code"],
            Harness::Agy => &["antigravity"],
        }
    }

    /// Whether the login vault (`src/vault`) can save and switch logins for seats of this harness.
    pub fn supports_vault(self) -> bool {
        match self {
            Harness::Claude | Harness::Agy => true,
        }
    }

    /// Whether this harness caches usage utilization / quota windows on disk.
    pub fn supports_limits(self) -> bool {
        match self {
            Harness::Claude => true,
            Harness::Agy => false,
        }
    }

    /// Whether `swarmy setup` can install `swarmy statusline` in seats of this harness.
    pub fn supports_statusline(self) -> bool {
        match self {
            Harness::Claude => true,
            Harness::Agy => false,
        }
    }

    /// Whether this harness deletes old transcripts automatically (constraining `keep_raw_days`).
    pub fn has_transcript_retention(self) -> bool {
        match self {
            Harness::Claude => true,
            Harness::Agy => false,
        }
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_capabilities() {
        for (h, tag) in Harness::ALL.iter().zip(Harness::TAGS) {
            assert_eq!(h.key(), tag);
            assert_eq!(Harness::from_key(tag), Some(*h));
            assert_eq!(Harness::from_key(h.short()), Some(*h));
        }
        assert_eq!(Harness::from_key("unknown"), None);
        assert!(Harness::Claude.supports_vault());
        assert!(Harness::Claude.supports_limits());
        assert!(Harness::Claude.supports_statusline());
        assert!(Harness::Claude.has_transcript_retention());
        assert!(Harness::Agy.supports_vault());
        assert!(!Harness::Agy.supports_limits());
        assert!(!Harness::Agy.supports_statusline());
        assert!(!Harness::Agy.has_transcript_retention());
    }
}
