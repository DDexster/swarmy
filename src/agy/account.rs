//! Read-only non-secret account identity for an Antigravity (`agy`) seat.
//!
//! Reads `~/.gemini/antigravity-cli/antigravity-oauth-token` and decodes ONLY the non-secret
//! `id_token` JWT payload (`sub`, `email`, `name`) and `auth_method` in memory. Raw access or
//! refresh tokens are never stored, logged or returned.

use std::path::Path;

use serde::Deserialize;

use crate::claude::config_json::Account;

#[derive(Deserialize, Default)]
#[serde(default)]
struct OAuthTokenFile {
    auth_method: Option<String>,
    id_token: Option<String>,
    id_token_email: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct IdTokenClaims {
    sub: Option<String>,
    email: Option<String>,
    name: Option<String>,
}

/// Reads the non-secret Google account identity from `seat_dir/antigravity-oauth-token`.
/// Missing files return `Ok(None)`; damaged JSON returns an error for `doctor`/`ATTENTION`.
pub fn read_account(seat_dir: &Path) -> Result<Option<Account>, String> {
    let path = seat_dir.join("antigravity-oauth-token");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{} is damaged: {e}", path.display()))?;
    Ok(account_of(&v))
}

/// The account a token file (or a saved copy of one) names: the non-secret `id_token` claims
/// and `id_token_email`, never the tokens.
pub fn account_of(v: &serde_json::Value) -> Option<Account> {
    let parsed = OAuthTokenFile::deserialize(v).ok()?;
    let claims = parsed
        .id_token
        .as_deref()
        .and_then(decode_jwt_payload)
        .and_then(|bytes| serde_json::from_slice::<IdTokenClaims>(&bytes).ok())
        .unwrap_or_default();
    let email = claims
        .email
        .filter(|s| !s.is_empty())
        .or_else(|| parsed.id_token_email.filter(|s| !s.is_empty()));
    // The email names the account (it is what the token file always carries); the subject only
    // when there is no email, so a newer agy writing an `id_token` does not split the account.
    let key = email
        .as_deref()
        .map(str::to_lowercase)
        .or_else(|| claims.sub.filter(|s| !s.is_empty()))?;
    let plan = parsed.auth_method.filter(|s| !s.is_empty());
    Some(Account {
        // Namespaced so Google subjects never collide with Anthropic UUIDs.
        account_uuid: format!("agy:{key}"),
        email,
        display_name: claims.name.filter(|s| !s.is_empty()),
        plan_display_name: plan,
        ..Default::default()
    })
}

/// Decodes the payload segment (second dot-separated part) of an unverified JWT.
fn decode_jwt_payload(jwt: &str) -> Option<Vec<u8>> {
    let mut parts = jwt.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _sig = parts.next()?;
    if parts.next().is_some() || payload.is_empty() {
        return None;
    }
    decode_base64url(payload)
}

/// The 6-bit value of a base64 (url-safe or standard) character.
fn sextet(b: u8) -> Option<u8> {
    if b.is_ascii_uppercase() {
        Some(b - b'A')
    } else if b.is_ascii_lowercase() {
        Some(b - b'a' + 26)
    } else if b.is_ascii_digit() {
        Some(b - b'0' + 52)
    } else if b == b'-' || b == b'+' {
        Some(62)
    } else if b == b'_' || b == b'/' {
        Some(63)
    } else {
        None
    }
}

fn decode_base64url(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;
    for b in input.bytes() {
        if b == b'=' {
            break;
        }
        let val = sextet(b)?;
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64url(data: &[u8]) -> String {
        const TBL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = chunk.get(1).copied().map(u32::from).unwrap_or(0);
            let b2 = chunk.get(2).copied().map(u32::from).unwrap_or(0);
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(TBL[((n >> 18) & 63) as usize] as char);
            out.push(TBL[((n >> 12) & 63) as usize] as char);
            if chunk.len() > 1 {
                out.push(TBL[((n >> 6) & 63) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(TBL[(n & 63) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn extracts_non_secret_claims_from_synthetic_token() {
        let t = tempfile::tempdir().unwrap();
        let header = b64url(br#"{"alg":"RS256"}"#);
        let payload =
            b64url(br#"{"sub":"1000000000001","email":"alex@example.com","name":"Alex Example"}"#);
        let jwt = format!("{header}.{payload}.sig");
        std::fs::write(
            t.path().join("antigravity-oauth-token"),
            serde_json::json!({
                "token": {"access_token": "secret-do-not-read"},
                "auth_method": "consumer",
                "id_token": jwt
            })
            .to_string(),
        )
        .unwrap();
        let acct = read_account(t.path()).unwrap().unwrap();
        assert_eq!(acct.account_uuid, "agy:alex@example.com");
        assert_eq!(acct.email.as_deref(), Some("alex@example.com"));
        assert_eq!(acct.display_name.as_deref(), Some("Alex Example"));
        assert_eq!(acct.plan_display_name.as_deref(), Some("consumer"));
    }

    #[test]
    fn reads_top_level_id_token_email_without_jwt() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().join("antigravity-oauth-token"),
            serde_json::json!({
                "token": {"access_token": "secret-do-not-read"},
                "auth_method": "consumer",
                "id_token_email": "sam@example.com"
            })
            .to_string(),
        )
        .unwrap();
        let acct = read_account(t.path()).unwrap().unwrap();
        assert_eq!(acct.account_uuid, "agy:sam@example.com");
        assert_eq!(acct.email.as_deref(), Some("sam@example.com"));
        assert_eq!(acct.plan_display_name.as_deref(), Some("consumer"));
    }

    #[test]
    fn missing_file_is_none_and_damaged_is_err() {
        let t = tempfile::tempdir().unwrap();
        assert!(read_account(t.path()).unwrap().is_none());
        std::fs::write(t.path().join("antigravity-oauth-token"), "{broken").unwrap();
        assert!(read_account(t.path()).is_err());
    }
}
