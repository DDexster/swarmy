//! The OAuth login of a seat: `claudeAiOauth` in `<config dir>/.credentials.json` (Linux and
//! Windows) or, on macOS, a Keychain item. Token values never leave this module except through
//! `Secret`, which does not print them.

use std::fmt;

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The verbatim `claudeAiOauth` object. Debug and Display show only the fingerprint.
#[derive(Clone, PartialEq)]
pub struct Secret(Value);

impl Secret {
    pub fn new(v: Value) -> Self {
        Secret(v)
    }

    pub fn expose(&self) -> &Value {
        &self.0
    }

    pub fn meta(&self) -> LoginMeta {
        LoginMeta::of(&self.0)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({})", self.meta().fingerprint)
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Best effort: overwrite token strings before the memory is freed.
        if let Value::Object(m) = &mut self.0 {
            for v in m.values_mut() {
                if let Value::String(s) = v {
                    wipe(std::mem::take(s));
                }
            }
        }
    }
}

/// Zeroes the heap buffer of a string before freeing it.
fn wipe(s: String) {
    let mut b = s.into_bytes();
    b.iter_mut().for_each(|x| *x = 0);
    std::hint::black_box(&b);
}

/// Non-secret facts about a login.
#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct LoginMeta {
    /// First 8 bytes of SHA-256 of the refresh token, hex: identifies the grant.
    pub fingerprint: String,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
    pub access_expires_at: Option<DateTime<Utc>>,
    pub refresh_expires_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
}

impl LoginMeta {
    pub fn of(v: &Value) -> LoginMeta {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let t = |k: &str| {
            v.get(k)
                .and_then(Value::as_i64)
                .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        };
        let token = v
            .get("refreshToken")
            .and_then(Value::as_str)
            .or_else(|| v.get("accessToken").and_then(Value::as_str));
        LoginMeta {
            fingerprint: token.map(fingerprint).unwrap_or_else(|| "none".into()),
            subscription_type: s("subscriptionType"),
            rate_limit_tier: s("rateLimitTier"),
            access_expires_at: t("expiresAt"),
            refresh_expires_at: t("refreshTokenExpiresAt"),
            scopes: v
                .get("scopes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

pub fn fingerprint(token: &str) -> String {
    let d = Sha256::digest(token.as_bytes());
    hex::encode(&d[..8])
}

/// Extracts `claudeAiOauth` from the text of a credentials file or of a backup holding just
/// `{"claudeAiOauth": {...}}`.
pub fn parse_login(text: &str) -> anyhow::Result<Option<Secret>> {
    let v: Value = serde_json::from_str(text)?;
    Ok(v.get("claudeAiOauth")
        .filter(|x| x.is_object())
        .cloned()
        .map(Secret))
}

/// Puts `login` into the credentials file text, keeping every other key (`mcpOAuth` tokens belong
/// to the config dir, not to the account, and must survive a switch).
pub fn with_login(text: Option<&str>, login: &Secret) -> anyhow::Result<String> {
    let mut m: serde_json::Map<String, Value> = match text {
        Some(t) if !t.trim().is_empty() => serde_json::from_str(t)?,
        _ => serde_json::Map::new(),
    };
    m.insert("claudeAiOauth".into(), login.expose().clone());
    Ok(serde_json::to_string(&m)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREDS: &str = r#"{"mcpOAuth":{"plugin:x|1":{"accessToken":"m"}},
      "claudeAiOauth":{"accessToken":"sk-a","refreshToken":"sk-r","expiresAt":1795377583375,
      "refreshTokenExpiresAt":1797969583375,"scopes":["user:inference"],"subscriptionType":"max",
      "rateLimitTier":"default_claude_max_20x"}}"#;

    #[test]
    fn parses_meta_without_exposing_tokens() {
        let s = parse_login(CREDS).unwrap().unwrap();
        let m = s.meta();
        assert_eq!(m.fingerprint, fingerprint("sk-r"));
        assert_eq!(m.fingerprint.len(), 16);
        assert_eq!(m.subscription_type.as_deref(), Some("max"));
        assert!(m.refresh_expires_at.is_some());
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("sk-a") && !dbg.contains("sk-r"), "{dbg}");
    }

    #[test]
    fn replacing_the_login_keeps_mcp_tokens() {
        let other = Secret::new(serde_json::json!({"accessToken": "sk-b", "refreshToken": "sk-q"}));
        let out = with_login(Some(CREDS), &other).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["claudeAiOauth"]["refreshToken"], "sk-q");
        assert_eq!(v["mcpOAuth"]["plugin:x|1"]["accessToken"], "m");
        let fresh = with_login(None, &other).unwrap();
        assert!(fresh.contains("sk-q"));
    }

    #[test]
    fn backup_files_hold_just_the_login() {
        assert!(
            parse_login(r#"{"claudeAiOauth":{"refreshToken":"x"}}"#)
                .unwrap()
                .is_some()
        );
        assert!(parse_login(r#"{"mcpOAuth":{}}"#).unwrap().is_none());
    }
}
