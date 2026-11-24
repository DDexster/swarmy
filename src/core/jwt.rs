//! The claims of a JSON Web Token, read without verifying it: the harnesses' login files carry
//! an `id_token` whose payload names the account (email, subject, plan). Only that payload is
//! decoded; the tokens themselves are secrets and never leave the vault.

/// Decodes the payload segment (second dot-separated part) of an unverified JWT.
pub fn payload(jwt: &str) -> Option<Vec<u8>> {
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

/// Base64url without padding: tests and the demo make (unsigned) tokens with it.
pub fn b64url(data: &[u8]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_payload_only() {
        let jwt = format!("{}.{}.sig", b64url(b"{}"), b64url(br#"{"sub":"s-1"}"#));
        assert_eq!(payload(&jwt).unwrap(), br#"{"sub":"s-1"}"#);
        assert!(payload("only.two").is_none());
        assert!(payload("a.b.c.d").is_none());
    }
}
