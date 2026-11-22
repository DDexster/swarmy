//! Vault behaviour on throwaway seats. Never touches a real config dir.

use serde_json::{Value, json};

use super::*;

fn seat(root: &Path, label: &str) -> Seat {
    let dir = root.join(format!(".claude-{label}"));
    std::fs::create_dir_all(&dir).unwrap();
    Seat {
        label: label.into(),
        claude_json: dir.join(".claude.json"),
        dir,
        is_default: false,
        found_by: vec![],
    }
}

fn write_seat(s: &Seat, refresh: &str, account: &str) {
    let creds = json!({
        "mcpOAuth": {"plugin:x|1": {"accessToken": format!("mcp-{}", s.label)}},
        "claudeAiOauth": {"accessToken": format!("acc-{refresh}"), "refreshToken": refresh, "expiresAt": 1_795_377_583_375_i64}
    });
    std::fs::write(s.credentials_file(), creds.to_string()).unwrap();
    let cj = json!({"numStartups": 7, "oauthAccount": {"accountUuid": account, "emailAddress": format!("{account}@example.com")}});
    std::fs::write(&s.claude_json, serde_json::to_string_pretty(&cj).unwrap()).unwrap();
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn capture_switch_and_back() {
    let t = tempfile::tempdir().unwrap();
    let work = seat(t.path(), "work");
    let spare = seat(t.path(), "spare");
    write_seat(&work, "r-alice", "alice");
    write_seat(&spare, "r-bob", "bob");
    let mut v = Vault::open(&t.path().join("vault")).unwrap();

    v.capture(&work, Some("alice")).unwrap();
    v.capture(&spare, Some("bob")).unwrap();
    assert_eq!(v.find("alice").unwrap().mounted.as_deref(), Some("work"));
    assert_eq!(
        v.find("bob").unwrap().email.as_deref(),
        Some("bob@example.com")
    );

    // bob is mounted in `spare`: refused without --share.
    let err = v.switch(&work, "bob", false).unwrap_err().to_string();
    assert!(err.contains("in use in seat 'spare'"), "{err}");

    // Free bob by putting a third login into `spare`.
    let carol = Secret::new(json!({"accessToken": "acc-c", "refreshToken": "r-carol"}));
    v.put(
        &carol,
        Some(json!({"accountUuid": "carol"})),
        None,
        Some("carol"),
        None,
    )
    .unwrap();
    v.switch(&spare, "carol", false).unwrap();
    assert_eq!(v.find("bob").unwrap().mounted, None);

    let rep = v.switch(&work, "bob", false).unwrap();
    assert_eq!(
        (rep.from.as_deref(), rep.to.as_str()),
        (Some("alice"), "bob")
    );
    let creds = read_json(&work.credentials_file());
    assert_eq!(creds["claudeAiOauth"]["refreshToken"], "r-bob");
    assert_eq!(
        creds["mcpOAuth"]["plugin:x|1"]["accessToken"], "mcp-work",
        "MCP tokens belong to the seat"
    );
    let cj = read_json(&work.claude_json);
    assert_eq!(cj["oauthAccount"]["accountUuid"], "bob");
    assert_eq!(
        cj["numStartups"], 7,
        "the rest of .claude.json is untouched"
    );
    assert_eq!(v.find("alice").unwrap().mounted, None);
    assert_eq!(v.find("bob").unwrap().mounted.as_deref(), Some("work"));
    let backups = std::fs::read_dir(rep.backup.unwrap()).unwrap().count();
    assert!(backups >= 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(work.credentials_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    // Back again.
    v.switch(&work, "alice", false).unwrap();
    assert_eq!(
        read_json(&work.credentials_file())["claudeAiOauth"]["refreshToken"],
        "r-alice"
    );
}

#[test]
fn rotated_tokens_are_taken_back_before_switching() {
    let t = tempfile::tempdir().unwrap();
    let work = seat(t.path(), "work");
    write_seat(&work, "r-alice-1", "alice");
    let mut v = Vault::open(&t.path().join("vault")).unwrap();
    v.capture(&work, Some("alice")).unwrap();
    let bob = Secret::new(json!({"refreshToken": "r-bob"}));
    v.put(
        &bob,
        Some(json!({"accountUuid": "bob"})),
        None,
        Some("bob"),
        None,
    )
    .unwrap();

    // Claude Code refreshes: the refresh token in the seat rotates.
    write_seat(&work, "r-alice-2", "alice");
    v.switch(&work, "bob", false).unwrap();
    let alice = v.find("alice").unwrap();
    assert_eq!(
        alice.meta.fingerprint,
        credentials::fingerprint("r-alice-2"),
        "the fresh token was saved, not the stale one"
    );
    assert_eq!(
        v.logins().len(),
        2,
        "rotation updates the entry instead of adding one"
    );
    assert_eq!(
        v.store.get(&alice.id).unwrap().unwrap().expose()["refreshToken"],
        "r-alice-2"
    );
}

#[test]
fn sync_follows_rotation_and_notices_foreign_logins() {
    let t = tempfile::tempdir().unwrap();
    let work = seat(t.path(), "work");
    write_seat(&work, "r1", "alice");
    let mut v = Vault::open(&t.path().join("vault")).unwrap();
    v.capture(&work, Some("alice")).unwrap();
    write_seat(&work, "r2", "alice");
    v.sync(std::slice::from_ref(&work)).unwrap();
    assert_eq!(
        v.find("alice").unwrap().meta.fingerprint,
        credentials::fingerprint("r2")
    );
    // The user ran /login with another account in that seat.
    write_seat(&work, "r9", "mallory");
    v.sync(std::slice::from_ref(&work)).unwrap();
    assert_eq!(v.find("alice").unwrap().mounted, None);
    assert_eq!(
        v.find("alice").unwrap().meta.fingerprint,
        credentials::fingerprint("r2"),
        "alice's tokens are kept"
    );
}

#[test]
fn remove_refuses_mounted_logins_and_secrets_never_print() {
    let t = tempfile::tempdir().unwrap();
    let work = seat(t.path(), "work");
    write_seat(&work, "r-secret-value", "alice");
    let mut v = Vault::open(&t.path().join("vault")).unwrap();
    v.capture(&work, Some("alice")).unwrap();
    assert!(v.remove("alice", false).is_err());
    let logins = std::fs::read_to_string(t.path().join("vault/logins.json")).unwrap();
    assert!(
        !logins.contains("r-secret-value"),
        "metadata file holds no tokens"
    );
    v.rename("alice", "personal").unwrap();
    assert!(v.find("personal").is_some());
    v.remove("personal", true).unwrap();
    assert!(v.logins().is_empty());
    let secrets = std::fs::read_to_string(t.path().join("vault/secrets.json")).unwrap();
    assert!(!secrets.contains("r-secret-value"));
}
