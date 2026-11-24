//! Agent harnesses (clients/runtimes) swarmy reads from disk.
//!
//! A seat is one config/data directory of a harness (for example `~/.claude`, `~/.claude-work`,
//! `~/.gemini/antigravity-cli`, `~/.codex`). Model providers (Anthropic, Google, OpenAI) are separate from
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
    /// OpenAI's Codex CLI.
    Codex,
}

impl Harness {
    /// Every harness swarmy knows, in display order.
    pub const ALL: [Harness; 3] = [Harness::Claude, Harness::Agy, Harness::Codex];

    /// Stable key stored in SQLite (`seats.harness`), JSON output and CLI flags.
    pub const TAGS: [&'static str; 3] = ["claude", "agy", "codex"];

    pub fn key(self) -> &'static str {
        match self {
            Harness::Claude => Self::TAGS[0],
            Harness::Agy => Self::TAGS[1],
            Harness::Codex => Self::TAGS[2],
        }
    }

    /// Compact 2–3 character badge for dense TUI rows when more than one harness is present.
    pub fn short(self) -> &'static str {
        match self {
            Harness::Claude => "cc",
            Harness::Agy => "agy",
            Harness::Codex => "cdx",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::Agy => "Antigravity",
            Harness::Codex => "Codex",
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
            Harness::Codex => &["openai-codex", "codex-cli"],
        }
    }

    /// Whether the login vault (`src/vault`) can save and switch logins for seats of this harness.
    pub fn supports_vault(self) -> bool {
        match self {
            Harness::Claude | Harness::Agy | Harness::Codex => true,
        }
    }

    /// Whether one saved login may be in several seats of this harness at once. Codex's refresh
    /// tokens are single-use (OpenAI refuses one used before), so two seats would sign each other
    /// out.
    pub fn shares_logins(self) -> bool {
        match self {
            Harness::Claude | Harness::Agy => true,
            Harness::Codex => false,
        }
    }

    /// Whether this harness caches usage utilization / quota windows on disk.
    pub fn supports_limits(self) -> bool {
        match self {
            Harness::Claude => true,
            Harness::Agy | Harness::Codex => false,
        }
    }

    /// Whether `swarmy setup` can install `swarmy statusline` in seats of this harness.
    pub fn supports_statusline(self) -> bool {
        match self {
            Harness::Claude => true,
            Harness::Agy | Harness::Codex => false,
        }
    }

    /// Whether this harness deletes old transcripts automatically (constraining `keep_raw_days`).
    pub fn has_transcript_retention(self) -> bool {
        match self {
            Harness::Claude => true,
            // Codex keeps its rollouts until the user deletes or archives them.
            Harness::Agy | Harness::Codex => false,
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
