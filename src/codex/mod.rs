//! OpenAI Codex CLI reader: homes (`~/.codex`, `CODEX_HOME`), rollouts (one per thread, append
//! only), thread names, open threads and the non-secret account of `auth.json`.
//!
//! Offline and read-only, like the other readers. Tokens are never logged.

pub mod account;
pub mod credentials;
pub mod discovery;
pub mod live;
pub mod meta;
pub mod rollout;
