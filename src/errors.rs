//! The catalogue of failures a user sees (review L3): each has a stable code, so a message can
//! be looked up, scripted against and tested, and callers can react to a kind of failure (the
//! dashboard tells "the vault is busy" from "the vault is damaged"). Errors are made where the
//! failure is found (`vault`, `config`, `cli::setup`, `index`, `app`) and travel inside
//! `anyhow::Error` to the edges, which print `swarmy: <message> [<code>]` ([`code_of`]).

use std::path::PathBuf;

/// The saved logins (`src/vault`).
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("another swarmy is changing the vault (still busy after {0:?})")]
    Locked(std::time::Duration),
    #[error("vault/{0} is damaged")]
    Damaged(&'static str),
    #[error("vault/logins.json was written by a newer swarmy (format {0}); update swarmy")]
    NewerFormat(u32),
    #[error("no login '{0}' in the vault (see `swarmy accounts`)")]
    NoLogin(String),
    #[error("'{key}' names several logins: {}; use the label", .logins.join(", "))]
    AmbiguousLogin { key: String, logins: Vec<String> },
    #[error("a login labelled '{0}' already exists")]
    LabelTaken(String),
    #[error("a label is needed")]
    LabelEmpty,
    #[error(
        "login '{login}' is in use in seat '{seat}'. One OAuth grant in two seats breaks one of them when the token rotates. Log in separately there (`swarmy accounts add`), or pass --share to copy it anyway."
    )]
    SharedGrant { login: String, seat: String },
    #[error(
        "login '{login}' is in use in seat '{seat}'; pass --force to forget it anyway (the seat keeps working)"
    )]
    InUse { login: String, seat: String },
    #[error("the secret of '{0}' is missing from the vault")]
    MissingSecret(String),
    #[error("on macOS Claude Code keeps the login in the Keychain; the vault cannot read it yet")]
    Keychain,
}

/// `config.toml`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid {path}: {message}")]
    Invalid { path: PathBuf, message: String },
    #[error("cannot read {path}: {source}")]
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// `swarmy setup` (Claude Code's `settings.json`).
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("{0} is not valid JSON; fix or remove it first (nothing was changed)")]
    SettingsDamaged(PathBuf),
}

/// The index (`src/index`).
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("the index was written by a newer swarmy ({what} version {version}); update swarmy")]
    NewerVersion { what: &'static str, version: u32 },
}

/// What a user named on the command line (`--account`, `--seat`).
#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error(
        "no account matches '{0}' (give an account uuid, an email or a saved login's label; `swarmy accounts` lists them)"
    )]
    NoAccount(String),
    #[error("'{key}' matches several accounts: {}; give the uuid", .candidates.join(", "))]
    AmbiguousAccount {
        key: String,
        candidates: Vec<String>,
    },
    #[error("no seat '{name}' (seats: {})", .known.join(", "))]
    NoSeat { name: String, known: Vec<String> },
}

/// A stable code per failure, for messages, scripts and tests.
pub trait Coded {
    fn code(&self) -> &'static str;
}

impl Coded for VaultError {
    fn code(&self) -> &'static str {
        match self {
            VaultError::Locked(_) => "vault-locked",
            VaultError::Damaged(_) => "vault-damaged",
            VaultError::NewerFormat(_) => "vault-newer",
            VaultError::NoLogin(_) => "vault-no-login",
            VaultError::AmbiguousLogin { .. } => "vault-ambiguous-login",
            VaultError::LabelTaken(_) => "vault-label-taken",
            VaultError::LabelEmpty => "vault-label-empty",
            VaultError::SharedGrant { .. } => "vault-shared-grant",
            VaultError::InUse { .. } => "vault-in-use",
            VaultError::MissingSecret(_) => "vault-missing-secret",
            VaultError::Keychain => "vault-keychain",
        }
    }
}

impl Coded for ConfigError {
    fn code(&self) -> &'static str {
        match self {
            ConfigError::Invalid { .. } => "config-invalid",
            ConfigError::Unreadable { .. } => "config-unreadable",
        }
    }
}

impl Coded for SetupError {
    fn code(&self) -> &'static str {
        match self {
            SetupError::SettingsDamaged(_) => "setup-settings-damaged",
        }
    }
}

impl Coded for IndexError {
    fn code(&self) -> &'static str {
        match self {
            IndexError::NewerVersion { .. } => "index-newer",
        }
    }
}

impl Coded for LookupError {
    fn code(&self) -> &'static str {
        match self {
            LookupError::NoAccount(_) => "no-account",
            LookupError::AmbiguousAccount { .. } => "ambiguous-account",
            LookupError::NoSeat { .. } => "no-seat",
        }
    }
}

/// The code of the first catalogued error in `e`'s chain.
pub fn code_of(e: &anyhow::Error) -> Option<&'static str> {
    e.chain().find_map(|c| {
        if let Some(x) = c.downcast_ref::<VaultError>() {
            Some(x.code())
        } else if let Some(x) = c.downcast_ref::<ConfigError>() {
            Some(x.code())
        } else if let Some(x) = c.downcast_ref::<SetupError>() {
            Some(x.code())
        } else if let Some(x) = c.downcast_ref::<IndexError>() {
            Some(x.code())
        } else {
            c.downcast_ref::<LookupError>().map(Coded::code)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_survive_context() {
        let e = anyhow::Error::from(VaultError::Locked(std::time::Duration::from_secs(30)))
            .context("cannot save the login");
        assert_eq!(code_of(&e), Some("vault-locked"));
        let e = anyhow::anyhow!("plain");
        assert_eq!(code_of(&e), None);
        let e: anyhow::Error = LookupError::AmbiguousAccount {
            key: "work".into(),
            candidates: vec!["a (u1)".into(), "b (u2)".into()],
        }
        .into();
        assert_eq!(code_of(&e), Some("ambiguous-account"));
        assert_eq!(
            e.to_string(),
            "'work' matches several accounts: a (u1), b (u2); give the uuid"
        );
    }

    /// Codes are part of the interface: each is unique.
    #[test]
    fn codes_are_unique() {
        let all = [
            VaultError::Locked(std::time::Duration::ZERO).code(),
            VaultError::Damaged("x").code(),
            VaultError::NewerFormat(3).code(),
            VaultError::NoLogin(String::new()).code(),
            VaultError::AmbiguousLogin {
                key: String::new(),
                logins: vec![],
            }
            .code(),
            VaultError::LabelTaken(String::new()).code(),
            VaultError::LabelEmpty.code(),
            VaultError::SharedGrant {
                login: String::new(),
                seat: String::new(),
            }
            .code(),
            VaultError::InUse {
                login: String::new(),
                seat: String::new(),
            }
            .code(),
            VaultError::MissingSecret(String::new()).code(),
            VaultError::Keychain.code(),
            ConfigError::Invalid {
                path: PathBuf::new(),
                message: String::new(),
            }
            .code(),
            ConfigError::Unreadable {
                path: PathBuf::new(),
                source: std::io::Error::other("x"),
            }
            .code(),
            SetupError::SettingsDamaged(PathBuf::new()).code(),
            IndexError::NewerVersion {
                what: "facts",
                version: 9,
            }
            .code(),
            LookupError::NoAccount(String::new()).code(),
            LookupError::AmbiguousAccount {
                key: String::new(),
                candidates: vec![],
            }
            .code(),
            LookupError::NoSeat {
                name: String::new(),
                known: vec![],
            }
            .code(),
        ];
        let set: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len());
    }
}
