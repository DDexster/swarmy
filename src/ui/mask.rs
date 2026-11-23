//! `--mask`: what is personal is hidden by its type when it is rendered, not by guessing from
//! its shape. An email is personal; so is a login label made from an email's local part (the
//! vault names a new login `alice` after `alice@example.com`, first review defect), which does
//! not look like an email at all.

/// What a masked value is shown as.
pub const HIDDEN: &str = "•••";

/// The local part of an email (`alice` of `alice@example.com`).
pub fn local_part(email: &str) -> Option<&str> {
    email
        .split_once('@')
        .map(|(l, _)| l)
        .filter(|l| !l.is_empty())
}

/// An email: hidden when masking.
pub fn email(e: &str, mask: bool) -> String {
    if mask { HIDDEN.into() } else { e.to_string() }
}

/// A login's label: hidden when masking if it was made from its email (the local part, or the
/// local part with the `-2`, `-3`… the vault adds to keep labels unique) or is an email itself.
pub fn login_label(label: &str, login_email: Option<&str>, mask: bool) -> String {
    if mask && from_email(label, login_email) {
        HIDDEN.into()
    } else {
        label.to_string()
    }
}

fn from_email(label: &str, login_email: Option<&str>) -> bool {
    if label.contains('@') {
        return true;
    }
    let Some(lp) = login_email.and_then(local_part) else {
        return false;
    };
    match label.strip_prefix(lp) {
        Some("") => true,
        Some(rest) => rest
            .strip_prefix('-')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
        None => false,
    }
}

/// Words a masked screen must not show anywhere, whatever drew them: the local parts of the
/// given emails (at least 3 characters, so short ones do not blank common words).
pub fn words<'a>(emails: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = emails
        .into_iter()
        .filter_map(local_part)
        .filter(|l| l.chars().count() >= 3)
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_made_from_emails_are_hidden() {
        let e = Some("alice@example.com");
        assert_eq!(login_label("alice", e, true), HIDDEN);
        assert_eq!(login_label("alice-2", e, true), HIDDEN);
        assert_eq!(login_label("alice", e, false), "alice");
        // A label the user chose stays: it carries no address.
        assert_eq!(login_label("work", e, true), "work");
        assert_eq!(login_label("alice-team", e, true), "alice-team");
        assert_eq!(login_label("alicex", e, true), "alicex");
        // An email as a label is hidden whatever the login's email is.
        assert_eq!(login_label("bob@example.com", None, true), HIDDEN);
        assert_eq!(login_label("login", None, true), "login");
        assert_eq!(email("alice@example.com", true), HIDDEN);
        assert_eq!(
            words([
                "alice@example.com",
                "al@example.com",
                "x",
                "alice@other.org"
            ]),
            ["alice"]
        );
    }
}
