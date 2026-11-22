//! Vault behaviour on throwaway seats. Never touches a real config dir.

use chrono::Utc;
use serde_json::{Value, json};

use super::*;

fn seat(root: &Path, label: &str) -> anyhow::Result<Seat> {
    let dir = root.join(format!(".claude-{label}"));
    std::fs::create_dir_all(&dir)?;
    Ok(Seat {
        label: label.into(),
        claude_json: dir.join(".claude.json"),
        dir,
        is_default: false,
        found_by: vec![],
    })
}

fn write_seat(s: &Seat, refresh: &str, account: &str) -> anyhow::Result<()> {
    let creds = json!({
        "mcpOAuth": {"plugin:x|1": {"accessToken": format!("mcp-{}", s.label)}},
        "claudeAiOauth": {"accessToken": format!("acc-{refresh}"), "refreshToken": refresh, "expiresAt": 1_795_377_583_375_i64}
    });
    std::fs::write(s.credentials_file(), creds.to_string())?;
    let cj = json!({"numStartups": 7, "oauthAccount": {"accountUuid": account, "emailAddress": format!("{account}@example.com")}});
    std::fs::write(&s.claude_json, serde_json::to_string_pretty(&cj)?)?;
    Ok(())
}

fn read_json(p: &Path) -> anyhow::Result<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(p)?)?)
}

fn find<'a>(v: &'a Vault, key: &str) -> Option<&'a Login> {
    v.index.find(key)
}

fn login<'a>(v: &'a Vault, key: &str) -> anyhow::Result<&'a Login> {
    find(v, key).with_context(|| format!("no login '{key}'"))
}

/// A saved secret, read from disk the way another process would see it.
fn secret_of(v: &Vault, id: &str) -> anyhow::Result<Value> {
    Ok(read_json(&v.dir.join("secrets.json"))?[id].clone())
}

fn secret(refresh: &str) -> Secret {
    Secret::new(json!({"accessToken": format!("acc-{refresh}"), "refreshToken": refresh}))
}

/// Files in `dir` whose names end with `suffix`.
fn count(dir: &Path, suffix: &str) -> usize {
    std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn capture_switch_and_back() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    let spare = seat(t.path(), "spare")?;
    write_seat(&work, "r-alice", "alice")?;
    write_seat(&spare, "r-bob", "bob")?;
    let mut v = Vault::open(&t.path().join("vault"))?;

    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    v.edit(|tx| tx.capture(&spare, Some("bob")))?;
    assert_eq!(login(&v, "alice")?.mounted.as_deref(), Some("work"));
    assert_eq!(login(&v, "bob")?.email.as_deref(), Some("bob@example.com"));

    // bob is mounted in `spare`: refused without --share.
    let err = v
        .edit(|tx| tx.switch(&work, "bob", false))
        .unwrap_err()
        .to_string();
    assert!(err.contains("in use in seat 'spare'"), "{err}");

    // Free bob by putting a third login into `spare`.
    let carol = Secret::new(json!({"accessToken": "acc-c", "refreshToken": "r-carol"}));
    v.edit(|tx| {
        tx.put(
            &carol,
            Some(json!({"accountUuid": "carol"})),
            None,
            Some("carol"),
            None,
        )
    })?;
    v.edit(|tx| tx.switch(&spare, "carol", false))?;
    assert_eq!(login(&v, "bob")?.mounted, None);

    let rep = v.edit(|tx| tx.switch(&work, "bob", false))?;
    assert_eq!(
        (rep.from.as_deref(), rep.to.as_str()),
        (Some("alice"), "bob")
    );
    let creds = read_json(&work.credentials_file())?;
    assert_eq!(creds["claudeAiOauth"]["refreshToken"], "r-bob");
    assert_eq!(
        creds["mcpOAuth"]["plugin:x|1"]["accessToken"], "mcp-work",
        "MCP tokens belong to the seat"
    );
    let cj = read_json(&work.claude_json)?;
    assert_eq!(cj["oauthAccount"]["accountUuid"], "bob");
    assert_eq!(
        cj["numStartups"], 7,
        "the rest of .claude.json is untouched"
    );
    assert_eq!(login(&v, "alice")?.mounted, None);
    assert_eq!(login(&v, "bob")?.mounted.as_deref(), Some("work"));
    let backups = std::fs::read_dir(rep.backup.context("no backup")?)?.count();
    assert!(backups >= 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(work.credentials_file())?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    // Back again.
    v.edit(|tx| tx.switch(&work, "alice", false))?;
    assert_eq!(
        read_json(&work.credentials_file())?["claudeAiOauth"]["refreshToken"],
        "r-alice"
    );
    Ok(())
}

fn write_creds(s: &Seat, access: &str, refresh: &str) -> anyhow::Result<()> {
    let creds =
        json!({"mcpOAuth": {}, "claudeAiOauth": {"accessToken": access, "refreshToken": refresh}});
    std::fs::write(s.credentials_file(), creds.to_string())?;
    Ok(())
}

#[test]
fn a_fresh_access_token_is_followed() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    // Claude Code refreshes the access token and keeps the refresh token.
    write_creds(&work, "acc-new", "r-alice")?;
    v.edit(|tx| tx.sync(std::slice::from_ref(&work)))?;
    let alice = login(&v, "alice")?.clone();
    assert_eq!(alice.mounted.as_deref(), Some("work"));
    assert_eq!(secret_of(&v, &alice.id)?["accessToken"], "acc-new");
    Ok(())
}

#[test]
fn a_new_login_never_overwrites_the_saved_one() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    v.edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?;
    // A backup of another account is copied in; the profile still names alice.
    write_creds(&work, "acc-bob", "r-bob")?;
    let states = v.edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?;
    let st = &states["work"];
    assert!(
        st.unverified_since.is_some(),
        "account not confirmed: {st:?}"
    );
    assert_eq!(st.account_uuid, None);
    assert_eq!(st.saved_as, None);
    let alice = login(&v, "alice")?.clone();
    assert_eq!(alice.mounted, None);
    assert_eq!(
        secret_of(&v, &alice.id)?["refreshToken"],
        "r-alice",
        "alice keeps her tokens"
    );
    // Saving it creates a separate login without borrowing alice's account.
    v.edit(|tx| tx.capture(&work, Some("bob")))?;
    let bob = login(&v, "bob")?;
    assert_eq!(bob.account_uuid, None);
    assert_eq!(bob.mounted.as_deref(), Some("work"));
    assert_eq!(v.logins().len(), 2);
    // Claude Code re-reads the profile: now the account is known.
    let cj = json!({"oauthAccount": {"accountUuid": "bob", "emailAddress": "bob@example.com", "profileFetchedAt": Utc::now().timestamp_millis()}});
    std::fs::write(&work.claude_json, cj.to_string())?;
    let st = &v.edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?["work"];
    assert_eq!(st.unverified_since, None);
    assert_eq!(st.account_uuid.as_deref(), Some("bob"));
    assert_eq!(st.saved_as.as_deref(), Some("bob"));
    Ok(())
}

#[test]
fn switching_away_from_an_unsaved_login_keeps_it() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    v.edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?;
    write_creds(&work, "acc-x", "r-unknown")?;
    let rep = v.edit(|tx| tx.switch(&work, "alice", false))?;
    assert_eq!(rep.auto_saved.as_deref(), Some("work"));
    assert_eq!(
        read_json(&work.credentials_file())?["claudeAiOauth"]["refreshToken"],
        "r-alice"
    );
    let kept = v
        .logins()
        .iter()
        .find(|l| l.label == "work")
        .expect("the unknown login was saved under the seat's name");
    assert_eq!(kept.account_uuid, None);
    assert_eq!(secret_of(&v, &kept.id)?["refreshToken"], "r-unknown");
    Ok(())
}

#[test]
fn remove_refuses_mounted_logins_and_secrets_never_print() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-secret-value", "alice")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    assert!(v.edit(|tx| tx.remove("alice", false)).is_err());
    let logins = std::fs::read_to_string(t.path().join("vault/logins.json"))?;
    assert!(
        !logins.contains("r-secret-value"),
        "metadata file holds no tokens"
    );
    v.edit(|tx| tx.rename("alice", "personal"))?;
    assert!(find(&v, "personal").is_some());
    v.edit(|tx| tx.remove("personal", true))?;
    assert!(v.logins().is_empty());
    let secrets = std::fs::read_to_string(t.path().join("vault/secrets.json"))?;
    assert!(!secrets.contains("r-secret-value"));
    Ok(())
}

#[test]
fn concurrent_changes_never_lose_a_login_or_corrupt_a_file() -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    const THREADS: usize = 8;
    const ROUNDS: usize = 40;
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    let seats: Vec<Seat> = (0..3)
        .map(|i| seat(t.path(), &format!("s{i}")))
        .collect::<anyhow::Result<_>>()?;
    for (i, s) in seats.iter().enumerate() {
        write_seat(s, &format!("r-seat{i}"), &format!("acct{i}"))?;
    }
    Vault::open(&vdir)?;
    let stop = AtomicBool::new(false);
    std::thread::scope(|sc| -> anyhow::Result<()> {
        // A lock-free reader always sees whole files.
        sc.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                for f in ["logins.json", "secrets.json"] {
                    if let Ok(text) = std::fs::read_to_string(vdir.join(f)) {
                        serde_json::from_str::<Value>(&text)
                            .unwrap_or_else(|e| panic!("{f} is damaged: {e}"));
                    }
                }
            }
        });
        // Claude Code keeps rewriting `.claude.json` meanwhile.
        let claude = sc.spawn(|| -> anyhow::Result<()> {
            let mut n = 0;
            while !stop.load(Ordering::Relaxed) {
                for s in &seats {
                    n += 1;
                    let mut cj = read_json(&s.claude_json)?;
                    cj["numStartups"] = json!(n);
                    store::atomic_write(&s.claude_json, cj.to_string().as_bytes())?;
                }
            }
            Ok(())
        });
        let workers: Vec<_> = (0..THREADS)
            .map(|th| {
                let (vdir, seats) = (&vdir, &seats);
                sc.spawn(move || -> anyhow::Result<()> {
                    let mut v = Vault::open(vdir)?;
                    for r in 0..ROUNDS {
                        let seat = &seats[(th + r) % seats.len()];
                        let step = r % 4;
                        if step == 0 {
                            let sec = secret(&format!("r-{th}-{r}"));
                            let label = format!("t{th}-{r}");
                            v.edit(|tx| tx.put(&sec, None, None, Some(&label), None))?;
                        } else if step == 1 {
                            v.edit(|tx| tx.capture(seat, None))?;
                        } else if step == 2 {
                            v.edit(|tx| tx.seat_states(seats))?;
                        } else {
                            let login = format!("t{th}-{}", r - 3);
                            v.edit(|tx| {
                                tx.sync(seats)?;
                                tx.switch(seat, &login, false)
                            })?;
                        }
                    }
                    Ok(())
                })
            })
            .collect();
        let mut result = Ok(());
        for w in workers {
            let r = w.join().map_err(|_| anyhow::anyhow!("a worker panicked"));
            result = result.and(r.and_then(|r| r));
        }
        stop.store(true, Ordering::Relaxed);
        result?;
        claude
            .join()
            .map_err(|_| anyhow::anyhow!("the writer panicked"))?
    })?;

    let v = Vault::open(&vdir)?;
    let secrets = read_json(&vdir.join("secrets.json"))?;
    for th in 0..THREADS {
        for r in (0..ROUNDS).step_by(4) {
            assert!(find(&v, &format!("t{th}-{r}")).is_some(), "t{th}-{r} lost");
        }
    }
    for l in v.logins() {
        let sec = Secret::new(secrets[&l.id].clone());
        assert_eq!(sec.meta().fingerprint, l.meta.fingerprint, "{}", l.label);
    }
    // The seats' first logins were saved before anything overwrote them.
    for i in 0..seats.len() {
        let refresh = format!("r-seat{i}");
        assert!(
            secrets
                .as_object()
                .context("secrets.json holds an object")?
                .values()
                .any(|s| s["refreshToken"] == refresh.as_str()),
            "{refresh} lost"
        );
    }
    for s in &seats {
        let mounted = v
            .logins()
            .iter()
            .filter(|l| l.mounted.as_deref() == Some(&s.label))
            .count();
        assert!(mounted <= 1, "{} logins mounted in {}", mounted, s.label);
    }
    let mut labels: Vec<&str> = v.logins().iter().map(|l| l.label.as_str()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), v.logins().len(), "labels are unique");
    assert_eq!(count(&vdir, ".tmp"), 0, "no temp file is left behind");
    Ok(())
}

#[test]
fn an_edit_rereads_the_vault_under_the_lock() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    // Both views are opened before either change.
    let mut first = Vault::open(&vdir)?;
    let mut second = Vault::open(&vdir)?;
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    std::thread::scope(|sc| -> anyhow::Result<()> {
        let holder = sc.spawn(|| {
            first.edit(|tx| {
                tx.put(&secret("r-a"), None, None, Some("a"), None)?;
                locked_tx.send(()).context("the main thread is gone")?;
                std::thread::sleep(std::time::Duration::from_millis(200));
                Ok(())
            })
        });
        locked_rx.recv()?;
        // Waits for the first edit, then builds on what it saved.
        second.edit(|tx| tx.put(&secret("r-b"), None, None, Some("b"), None))?;
        holder
            .join()
            .map_err(|_| anyhow::anyhow!("the first edit panicked"))?
    })?;
    let v = Vault::open(&vdir)?;
    assert!(find(&v, "a").is_some() && find(&v, "b").is_some());
    assert_eq!(secret_of(&v, &login(&v, "a")?.id)?["refreshToken"], "r-a");
    assert_eq!(second.logins().len(), 2, "the view shows the saved vault");
    Ok(())
}

#[test]
fn a_lock_file_left_behind_does_not_block() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    std::fs::create_dir_all(&vdir)?;
    // What an older swarmy or a crashed process may leave: a pid that is not running.
    std::fs::write(vdir.join("lock"), "4194300")?;
    let mut v = Vault::open(&vdir)?;
    v.edit(|tx| tx.put(&secret("r-a"), None, None, Some("a"), None))?;
    v.edit(|tx| tx.rename("a", "b"))?;
    assert!(find(&v, "b").is_some());
    Ok(())
}

#[test]
fn a_failed_edit_saves_nothing() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    let res: anyhow::Result<()> = v.edit(|tx| {
        tx.put(&secret("r-a"), None, None, Some("a"), None)?;
        bail!("changed my mind")
    });
    assert!(res.is_err());
    assert!(v.logins().is_empty());
    assert!(!v.dir.join("logins.json").exists());
    assert!(!v.dir.join("secrets.json").exists());
    Ok(())
}

#[test]
fn switch_keeps_what_claude_code_wrote_meanwhile() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let path = t.path().join(".claude.json");
    std::fs::write(
        &path,
        r#"{"numStartups": 7, "oauthAccount": {"accountUuid": "a"}}"#,
    )?;
    let calls = std::cell::Cell::new(0);
    let wrote = store::rewrite_merging(&path, |text| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            // Claude Code replaces the file while swarmy prepares its version.
            let newer = r#"{"numStartups": 8, "projects": {"p": {}}, "oauthAccount": {"accountUuid": "a"}}"#;
            store::atomic_write(&path, newer.as_bytes())?;
        }
        config_json::with_oauth_account(text, Some(&json!({"accountUuid": "b"})))
    })?;
    assert!(wrote);
    assert_eq!(
        calls.get(),
        2,
        "the change is applied again to the newer text"
    );
    let cj = read_json(&path)?;
    assert_eq!(cj["oauthAccount"]["accountUuid"], "b");
    assert_eq!(cj["numStartups"], 8);
    assert!(cj["projects"]["p"].is_object());
    assert!(!store::rewrite_merging(
        &t.path().join("missing.json"),
        |t| Ok(t.into())
    )?);
    Ok(())
}

#[test]
fn labels_stay_unique() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.put(&secret("r-bob"), None, None, Some("bob"), None))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    // Re-labelling an existing login with another login's label is refused everywhere.
    let err = v
        .edit(|tx| tx.capture(&work, Some("bob")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("'bob' already exists"), "{err}");
    let err = v
        .edit(|tx| tx.put(&secret("r-alice"), None, None, Some("bob"), None))
        .unwrap_err()
        .to_string();
    assert!(err.contains("'bob' already exists"), "{err}");
    assert!(v.edit(|tx| tx.rename("alice", "bob")).is_err());
    assert!(v.edit(|tx| tx.rename("alice", " ")).is_err());
    assert_eq!(
        login(&v, "alice")?.meta.fingerprint,
        secret("r-alice").meta().fingerprint
    );
    // Its own label is fine.
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    // Without a label a new login gets a free one.
    v.edit(|tx| tx.put(&secret("r-bob2"), None, None, None, None))?;
    v.edit(|tx| tx.put(&secret("r-x"), None, None, None, None))?;
    assert!(find(&v, "login").is_some() && find(&v, "login-2").is_some());
    // Re-saving an unconfirmed login keeps its label instead of renaming it.
    write_creds(&work, "acc-u", "r-unknown")?;
    v.edit(|tx| tx.capture(&work, None))?;
    v.edit(|tx| tx.capture(&work, None))?;
    assert!(find(&v, "work").is_some() && find(&v, "work-2").is_none());
    Ok(())
}

#[test]
fn duplicate_labels_from_older_versions_are_repaired() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    let mut v = Vault::open(&vdir)?;
    v.edit(|tx| tx.put(&secret("r-a"), None, None, Some("a"), None))?;
    v.edit(|tx| tx.put(&secret("r-b"), None, None, Some("b"), None))?;
    let mut idx = read_json(&vdir.join("logins.json"))?;
    idx["logins"][1]["label"] = json!("a");
    std::fs::write(vdir.join("logins.json"), idx.to_string())?;
    let mut v = Vault::open(&vdir)?;
    assert_eq!(
        login(&v, "a")?.meta.fingerprint,
        secret("r-a").meta().fingerprint
    );
    v.edit(|_| Ok(()))?;
    assert_eq!(
        login(&v, "a-2")?.meta.fingerprint,
        secret("r-b").meta().fingerprint
    );
    Ok(())
}

#[test]
fn remove_saves_the_index_before_deleting_the_secret() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    let mut v = Vault::open(&vdir)?;
    v.edit(|tx| tx.put(&secret("r-a"), None, None, Some("a"), None))?;
    let id = login(&v, "a")?.id.clone();
    // Make the secrets file impossible to replace: only the index can be written.
    let res = v.edit(|tx| {
        tx.remove("a", false)?;
        std::fs::remove_file(vdir.join("secrets.json"))?;
        std::fs::create_dir(vdir.join("secrets.json"))?;
        Ok(())
    });
    assert!(res.is_err());
    let idx = read_json(&vdir.join("logins.json"))?;
    assert_eq!(
        idx["logins"].as_array().context("a list")?.len(),
        0,
        "the index went first"
    );
    std::fs::remove_dir(vdir.join("secrets.json"))?;
    // The normal path: both are gone.
    let mut v = Vault::open(&vdir)?;
    v.edit(|tx| tx.put(&secret("r-a"), None, None, Some("a"), None))?;
    v.edit(|tx| tx.remove("a", false))?;
    assert!(read_json(&vdir.join("secrets.json"))?.get(&id).is_none());
    Ok(())
}

#[test]
fn the_vault_backs_up_its_own_files_bounded() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    let bdir = vdir.join("backups/vault");
    let mut v = Vault::open(&vdir)?;
    v.edit(|tx| tx.put(&secret("r-a"), None, None, Some("a0"), None))?;
    assert_eq!(count(&bdir, "-logins.json"), 0, "nothing to back up yet");
    v.edit(|tx| tx.put(&secret("r-b"), None, None, Some("b"), None))?;
    assert_eq!(count(&bdir, "-logins.json"), 1);
    assert_eq!(count(&bdir, "-secrets.json"), 1);
    let before = std::fs::read_dir(&bdir)?.count();
    v.edit(|tx| tx.seat_states(&[]))?;
    assert_eq!(
        std::fs::read_dir(&bdir)?.count(),
        before,
        "no change, no backup"
    );
    for i in 1..=VAULT_BACKUPS + 5 {
        let (old, new) = (format!("a{}", i - 1), format!("a{i}"));
        v.edit(|tx| tx.rename(&old, &new))?;
    }
    assert_eq!(count(&bdir, "-logins.json"), VAULT_BACKUPS);
    assert_eq!(
        count(&bdir, "-secrets.json"),
        1,
        "only changed files are copied"
    );
    // The newest copy holds the state before the last change.
    let mut names: Vec<_> = std::fs::read_dir(&bdir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with("-logins.json"))
        .collect();
    names.sort();
    let last = read_json(names.last().context("a backup")?)?;
    assert!(
        last.to_string()
            .contains(&format!("a{}", VAULT_BACKUPS + 4))
    );
    Ok(())
}

#[test]
fn seat_backups_are_kept_per_seat_and_never_overwrite() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    let spare = seat(t.path(), "spare")?;
    write_seat(&work, "r-alice", "alice")?;
    write_seat(&spare, "r-bob", "bob")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    v.edit(|tx| tx.capture(&spare, Some("bob")))?;
    for name in ["carol", "dave"] {
        let acct = json!({"accountUuid": name});
        v.edit(|tx| {
            tx.put(
                &secret(&format!("r-{name}")),
                Some(acct),
                None,
                Some(name),
                None,
            )
        })?;
    }
    let rep = v.edit(|tx| tx.switch(&spare, "dave", false))?;
    let spare_dir = rep.backup.context("no backup")?;
    // Three switches within one second: three copies.
    for login in ["carol", "alice", "carol"] {
        v.edit(|tx| tx.switch(&work, login, false))?;
    }
    let work_dir = t.path().join("vault/backups/seats/work");
    assert_eq!(count(&work_dir, "-credentials.json"), 3);
    assert_eq!(count(&work_dir, "-claude.json"), 3);
    for i in 0..SEAT_BACKUPS + 5 {
        let login = if i % 2 == 0 { "alice" } else { "carol" };
        v.edit(|tx| tx.switch(&work, login, false))?;
    }
    assert_eq!(count(&work_dir, "-credentials.json"), SEAT_BACKUPS);
    assert_eq!(count(&work_dir, "-claude.json"), SEAT_BACKUPS);
    assert_eq!(
        count(&spare_dir, "-credentials.json"),
        1,
        "a rarely switched seat keeps its backup"
    );
    Ok(())
}
