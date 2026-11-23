//! Vault behaviour on throwaway seats. Never touches a real config dir.

use chrono::Utc;
use serde_json::{Value, json};

use super::*;

fn seat(root: &Path, label: &str) -> anyhow::Result<Seat> {
    let dir = root.join(format!(".claude-{label}"));
    std::fs::create_dir_all(&dir)?;
    Ok(Seat::new(
        label,
        dir.clone(),
        dir.join(".claude.json"),
        false,
    ))
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
    v.index.find(key).ok()
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
    assert_eq!(login(&v, "alice")?.mounted.as_ref(), Some(&work.id));
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
    assert_eq!(login(&v, "bob")?.mounted.as_ref(), Some(&work.id));
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
    assert_eq!(alice.mounted.as_ref(), Some(&work.id));
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
    let st = &states[&work.id];
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
    assert_eq!(bob.mounted.as_ref(), Some(&work.id));
    assert_eq!(v.logins().len(), 2);
    // Claude Code re-reads the profile: now the account is known.
    let cj = json!({"oauthAccount": {"accountUuid": "bob", "emailAddress": "bob@example.com", "profileFetchedAt": Utc::now().timestamp_millis()}});
    std::fs::write(&work.claude_json, cj.to_string())?;
    let st = &v.edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?[&work.id];
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
    // Enough to interleave every kind of change many times, small enough that no edit waits
    // anywhere near `LOCK_TIMEOUT` on a loaded machine (review L4: 8 × 40 once timed out).
    const THREADS: usize = 4;
    const ROUNDS: usize = 16;
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
                        let done = if step == 0 {
                            let sec = secret(&format!("r-{th}-{r}"));
                            let label = format!("t{th}-{r}");
                            v.edit(|tx| tx.put(&sec, None, None, Some(&label), None))
                                .map(drop)
                        } else if step == 1 {
                            v.edit(|tx| tx.capture(seat, None)).map(drop)
                        } else if step == 2 {
                            v.edit(|tx| tx.seat_states(seats)).map(drop)
                        } else {
                            let login = format!("t{th}-{}", r - 3);
                            v.edit(|tx| {
                                tx.sync(seats)?;
                                tx.switch(seat, &login, false)
                            })
                            .map(drop)
                        };
                        // Says which change failed and why, so a failure is never a mystery.
                        done.with_context(|| format!("thread {th}, round {r}, step {step}"))?;
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
            .filter(|l| l.mounted.as_ref() == Some(&s.id))
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
        anyhow::bail!("changed my mind")
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
    // Kept under the seat's id: a new label changes nothing.
    let work_dir = t.path().join("vault/backups/seats").join(work.id.as_str());
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

/// Review H3: a vault an older swarmy wrote names seats by label (`mounted: "work"`, `seats`
/// keyed by label, no `v`). It is read in place, nothing is lost, and the labels become seat ids
/// the first time the seat is seen; a label no seat carries waits. Afterwards a new label of the
/// seat changes nothing.
#[test]
fn an_old_vault_keyed_by_labels_moves_to_seat_ids() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let vdir = t.path().join("vault");
    std::fs::create_dir_all(&vdir)?;
    // What the version before seat ids wrote (fingerprints as `credentials::LoginMeta` makes them).
    let fp = |r: &str| secret(r).meta().fingerprint;
    let login = |id: &str, label: &str, refresh: &str, mounted: &str| {
        json!({"id": id, "label": label, "account_uuid": label, "email": null, "plan": null,
               "meta": serde_json::to_value(secret(refresh).meta()).unwrap(),
               "oauth_account": null, "captured_at": "2026-11-01T00:00:00Z",
               "updated_at": "2026-11-01T00:00:00Z", "mounted": mounted})
    };
    let old = json!({
        "logins": [login("l1", "alice", "r-alice", "work"), login("l2", "bob", "r-bob", "gone")],
        "seats": {
            "work": {"fingerprint": fp("r-alice"), "account_uuid": "alice", "profile_at": null, "unverified_since": null},
            "gone": {"fingerprint": fp("r-bob"), "account_uuid": "bob", "profile_at": null, "unverified_since": null}
        }
    });
    std::fs::write(
        vdir.join("logins.json"),
        serde_json::to_string_pretty(&old)?,
    )?;
    let secrets = json!({"l1": secret("r-alice").expose(), "l2": secret("r-bob").expose()});
    std::fs::write(vdir.join("secrets.json"), secrets.to_string())?;

    // Read as it is: the labels are kept aside, not taken for ids.
    let mut v = Vault::open(&vdir)?;
    assert_eq!(v.logins().len(), 2);
    assert!(v.logins().iter().all(|l| l.mounted.is_none()));
    assert_eq!(v.index.logins[0].mounted_label.as_deref(), Some("work"));

    // The first change that sees the seat adopts its label.
    let states = v.edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?;
    assert_eq!(states[&work.id].saved_as.as_deref(), Some("alice"));
    let alice = login_of(&v, "alice")?;
    assert_eq!(alice.mounted.as_ref(), Some(&work.id));
    assert_eq!(alice.mounted_label, None);
    let bob = login_of(&v, "bob")?;
    assert_eq!(bob.mounted, None);
    assert_eq!(
        bob.mounted_label.as_deref(),
        Some("gone"),
        "waits for its seat"
    );
    let disk = read_json(&vdir.join("logins.json"))?;
    assert_eq!(disk["v"], 3);
    assert!(disk["seats"].get(work.id.as_str()).is_some(), "{disk}");
    assert!(disk["seats"].get("work").is_none());
    assert!(
        disk["legacy_seats"].get("gone").is_some(),
        "nothing dropped: {disk}"
    );
    // Both secrets are kept; alice's copy follows the seat (the mount was recognised).
    let disk_secrets = read_json(&vdir.join("secrets.json"))?;
    assert_eq!(disk_secrets["l2"], secrets["l2"]);
    assert_eq!(disk_secrets["l1"]["refreshToken"], "r-alice");
    assert_eq!(disk_secrets["l1"]["expiresAt"], 1_795_377_583_375_i64);
    // A login still mounted under an old label is not forgotten by accident.
    assert!(v.edit(|tx| tx.remove("bob", false)).is_err());

    // The seat gets another label: the vault still knows what is in it.
    let job = Seat::new("job", work.dir.clone(), work.claude_json.clone(), false);
    assert_eq!(job.id, work.id);
    v.edit(|tx| tx.capture(&job, None))?;
    assert_eq!(v.logins().len(), 2, "the same login, not a new one");
    assert_eq!(login_of(&v, "alice")?.mounted.as_ref(), Some(&work.id));
    let states = v.edit(|tx| tx.seat_states(std::slice::from_ref(&job)))?;
    assert_eq!(states[&job.id].unverified_since, None);
    Ok(())
}

fn login_of(v: &Vault, label: &str) -> anyhow::Result<Login> {
    login(v, label).cloned()
}

/// Review L3: a key that names two logins is an error naming both, never the first match.
#[test]
fn a_key_naming_two_logins_is_ambiguous() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    let acct = |uuid: &str, email: &str| crate::claude::config_json::Account {
        account_uuid: uuid.into(),
        email: Some(email.into()),
        ..Default::default()
    };
    v.edit(|tx| {
        tx.put(
            &secret("r-1"),
            None,
            Some(&acct("u1", "a@example.com")),
            Some("first"),
            None,
        )?;
        // A label equal to the other login's email.
        tx.put(
            &secret("r-2"),
            None,
            Some(&acct("u2", "b@example.com")),
            Some("a@example.com"),
            None,
        )
    })?;
    let e = v.edit(|tx| tx.rename("a@example.com", "x")).unwrap_err();
    assert_eq!(crate::errors::code_of(&e), Some("vault-ambiguous-login"));
    let msg = e.to_string();
    assert!(
        msg.contains("first") && msg.contains("a@example.com"),
        "{msg}"
    );
    // Unique keys still work: a label, an id, an email.
    assert_eq!(login(&v, "first")?.account_uuid.as_deref(), Some("u1"));
    assert_eq!(login(&v, "b@example.com")?.label, "a@example.com");
    let e = v.edit(|tx| tx.remove("nobody", false)).unwrap_err();
    assert_eq!(crate::errors::code_of(&e), Some("vault-no-login"));
    let e = v.edit(|tx| tx.rename("first", "")).unwrap_err();
    assert_eq!(crate::errors::code_of(&e), Some("vault-label-empty"));
    Ok(())
}

/// Test-only hooks into the middle of a switch.
pub(super) mod hooks {
    use std::cell::RefCell;

    type Hook = Box<dyn FnMut()>;

    thread_local! {
        static BEFORE_SEAT_WRITE: RefCell<Option<Hook>> = RefCell::new(None);
    }

    /// Runs the hook set on this thread, right before a switch writes the seat's credentials.
    pub(in crate::vault) fn before_seat_write() {
        BEFORE_SEAT_WRITE.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f();
            }
        });
    }

    pub(super) fn set_before_seat_write(f: Option<Hook>) {
        BEFORE_SEAT_WRITE.with(|h| *h.borrow_mut() = f);
    }
}

#[cfg(unix)]
/// A seat whose `.claude.json` lives outside its dir (as the default seat's does, in `$HOME`).
fn seat_with_home(root: &Path, label: &str) -> anyhow::Result<(Seat, PathBuf)> {
    let dir = root.join(format!(".claude-{label}"));
    let home = root.join(format!("home-{label}"));
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(&home)?;
    Ok((
        Seat::new(label, dir.clone(), home.join(".claude.json"), false),
        home,
    ))
}

#[cfg(unix)]
fn chmod(p: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Review H4: the `.claude.json` write fails after the credentials were written (here: its dir
/// cannot be written, as `$HOME` may not be for the default seat). The seat goes back to what it
/// held, and the vault still says so: the login stays mounted, the target stays free.
#[cfg(unix)]
#[test]
fn a_switch_that_fails_half_way_puts_the_seat_back() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let (work, home) = seat_with_home(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    v.edit(|tx| {
        tx.put(
            &secret("r-bob"),
            Some(json!({"accountUuid": "bob"})),
            None,
            Some("bob"),
            None,
        )
    })?;
    let creds_before = std::fs::read_to_string(work.credentials_file())?;
    chmod(&home, 0o500)?;
    if std::fs::write(home.join("probe"), "").is_ok() {
        // Running as root: permissions do not stop the write, nothing to prove here.
        chmod(&home, 0o700)?;
        return Ok(());
    }
    let res = v.edit(|tx| tx.switch(&work, "bob", false));
    chmod(&home, 0o700)?;
    let err = format!("{:#}", res.err().context("the switch had to fail")?);
    assert!(err.contains("the seat is as it was"), "{err}");
    assert_eq!(
        std::fs::read_to_string(work.credentials_file())?,
        creds_before,
        "the credentials are back as they were"
    );
    assert_eq!(
        read_json(&work.claude_json)?["oauthAccount"]["accountUuid"],
        "alice"
    );
    assert_eq!(login(&v, "alice")?.mounted.as_ref(), Some(&work.id));
    assert_eq!(login(&v, "bob")?.mounted, None);
    // Nothing is half done: the same switch works once the file can be written.
    v.edit(|tx| tx.switch(&work, "bob", false))?;
    assert_eq!(
        read_json(&work.credentials_file())?["claudeAiOauth"]["refreshToken"],
        "r-bob"
    );
    assert_eq!(
        read_json(&work.claude_json)?["oauthAccount"]["accountUuid"],
        "bob"
    );
    Ok(())
}

/// Review H4: a switch that died between its two writes (or a seat file put back by hand)
/// leaves a saved login in a seat while the vault calls it free. The next sync mounts it there,
/// so the one-grant-one-seat guard still holds.
#[test]
fn a_seat_holding_a_free_saved_login_gets_it_mounted() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    let spare = seat(t.path(), "spare")?;
    write_seat(&work, "r-alice", "alice")?;
    write_seat(&spare, "r-carol", "carol")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    v.edit(|tx| tx.capture(&spare, Some("carol")))?;
    v.edit(|tx| tx.put(&secret("r-bob"), None, None, Some("bob"), None))?;
    // The credentials are bob's, `.claude.json` still names alice.
    write_creds(&work, "acc-r-bob", "r-bob")?;
    let both = [work.clone(), spare.clone()];
    v.edit(|tx| tx.sync(&both))?;
    assert_eq!(login(&v, "bob")?.mounted.as_ref(), Some(&work.id));
    assert_eq!(login(&v, "alice")?.mounted, None);
    let e = v
        .edit(|tx| tx.switch(&spare, "bob", false))
        .err()
        .context("bob is in `work`: refused without --share")?;
    assert_eq!(crate::errors::code_of(&e), Some("vault-shared-grant"));
    Ok(())
}

/// Review M7: Claude Code refreshes the seat's token while a switch runs (after swarmy saved the
/// seat's login, before it replaced the file). The newer token is saved first, never lost.
#[test]
fn a_token_refreshed_during_a_switch_is_saved_not_overwritten() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    v.edit(|tx| tx.put(&secret("r-bob"), None, None, Some("bob"), None))?;
    let creds = work.credentials_file();
    let mut fired = false;
    hooks::set_before_seat_write(Some(Box::new(move || {
        if !fired {
            fired = true;
            let refreshed = json!({"mcpOAuth": {"plugin:x|1": {"accessToken": "mcp-new"}},
                "claudeAiOauth": {"accessToken": "acc-refreshed", "refreshToken": "r-alice"}});
            if let Err(e) = store::atomic_write(&creds, refreshed.to_string().as_bytes()) {
                panic!("cannot simulate Claude Code: {e}");
            }
        }
    })));
    let res = v.edit(|tx| tx.switch(&work, "bob", false));
    hooks::set_before_seat_write(None);
    res?;
    let alice = login(&v, "alice")?.clone();
    assert_eq!(
        secret_of(&v, &alice.id)?["accessToken"],
        "acc-refreshed",
        "the token Claude Code wrote meanwhile was saved"
    );
    let now = read_json(&work.credentials_file())?;
    assert_eq!(now["claudeAiOauth"]["refreshToken"], "r-bob");
    assert_eq!(
        now["mcpOAuth"]["plugin:x|1"]["accessToken"], "mcp-new",
        "the MCP token written meanwhile is kept too"
    );
    Ok(())
}

/// Review M7 (defects): a file another program keeps rewriting is never overwritten blindly
/// (the eighth attempt used to write anyway), and a torn read is read again instead of failing
/// the change half way.
#[test]
fn merging_writes_never_go_over_an_unseen_version() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let path = t.path().join(".claude.json");
    std::fs::write(&path, r#"{"n": 0, "oauthAccount": {"accountUuid": "a"}}"#)?;
    let n = std::cell::Cell::new(0);
    let res = store::rewrite_merging(&path, |text| {
        n.set(n.get() + 1);
        // Claude Code writes a newer version every time swarmy has read one.
        let newer = format!(
            r#"{{"n": {}, "oauthAccount": {{"accountUuid": "a"}}}}"#,
            n.get()
        );
        store::atomic_write(&path, newer.as_bytes())?;
        config_json::with_oauth_account(text, Some(&json!({"accountUuid": "b"})))
    });
    let e = res
        .err()
        .context("a file that never holds still is not written")?;
    assert_eq!(crate::errors::code_of(&e), Some("vault-seat-busy"));
    let cj = read_json(&path)?;
    assert_eq!(cj["n"], n.get(), "the other writer's last version stands");
    assert_eq!(cj["oauthAccount"]["accountUuid"], "a");

    let calls = std::cell::Cell::new(0);
    let wrote = store::rewrite_merging(&path, |text| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            anyhow::bail!("EOF while parsing (Claude Code is mid-write)");
        }
        config_json::with_oauth_account(text, Some(&json!({"accountUuid": "b"})))
    })?;
    assert!(wrote);
    assert_eq!(read_json(&path)?["oauthAccount"]["accountUuid"], "b");

    // Replaced only while it holds exactly what was read.
    let text = std::fs::read_to_string(&path)?;
    assert!(!store::replace_if_unchanged(
        &path,
        Some("{}"),
        Some(b"{\"x\": 1}")
    )?);
    assert_eq!(std::fs::read_to_string(&path)?, text);
    assert!(store::replace_if_unchanged(
        &path,
        Some(&text),
        Some(b"{\"x\": 1}")
    )?);
    assert_eq!(read_json(&path)?["x"], 1);
    assert_eq!(count(t.path(), ".tmp"), 0, "no temp file is left behind");
    Ok(())
}

/// Review M6: Claude Code keeps the refresh token when it refreshes (verified on 2.1.329), so a
/// login is named by it. Should a refresh ever rotate it, the login keeps its identity: same
/// account and plan, a fresh access token written by Claude Code. The saved copy follows, the
/// login stays mounted, the seat stays confirmed, and switching back writes the live grant. A
/// copied-in file with a stale access token is still another login.
#[test]
fn a_rotated_refresh_token_keeps_the_login() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let mut v = Vault::open(&t.path().join("vault"))?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    v.edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?;
    let write = |refresh: &str, expires: chrono::DateTime<Utc>| -> anyhow::Result<()> {
        let creds = json!({"mcpOAuth": {}, "claudeAiOauth": {
            "accessToken": format!("acc-{refresh}"), "refreshToken": refresh,
            "expiresAt": expires.timestamp_millis()}});
        std::fs::write(work.credentials_file(), creds.to_string())?;
        Ok(())
    };
    write("r-alice-2", Utc::now() + chrono::Duration::hours(8))?;
    let states = v.edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?;
    let st = &states[&work.id];
    assert_eq!(st.unverified_since, None, "{st:?}");
    assert_eq!(st.account_uuid.as_deref(), Some("alice"));
    assert_eq!(st.saved_as.as_deref(), Some("alice"));
    let alice = login(&v, "alice")?.clone();
    assert_eq!(alice.mounted.as_ref(), Some(&work.id));
    assert_eq!(secret_of(&v, &alice.id)?["refreshToken"], "r-alice-2");
    assert_eq!(v.logins().len(), 1, "no second login for the same grant");

    // Away and back: the live, rotated grant goes back into the seat.
    v.edit(|tx| {
        tx.put(
            &secret("r-bob"),
            Some(json!({"accountUuid": "bob"})),
            None,
            Some("bob"),
            None,
        )
    })?;
    v.edit(|tx| tx.switch(&work, "bob", false))?;
    v.edit(|tx| tx.switch(&work, "alice", false))?;
    assert_eq!(
        read_json(&work.credentials_file())?["claudeAiOauth"]["refreshToken"],
        "r-alice-2"
    );
    assert_eq!(v.logins().len(), 2);

    // An old copy of the same account (its access token long expired) is not a rotation.
    write("r-alice-old", Utc::now() - chrono::Duration::hours(1))?;
    v.edit(|tx| tx.sync(std::slice::from_ref(&work)))?;
    let alice = login(&v, "alice")?.clone();
    assert_eq!(alice.mounted, None);
    assert_eq!(secret_of(&v, &alice.id)?["refreshToken"], "r-alice-2");
    Ok(())
}

/// Review L4: reading the seats' state takes no lock and writes nothing, so a report or the
/// dashboard never waits for (or fails on) another swarmy's change. The dashboard does the
/// bookkeeping on the side, only when the vault is free.
#[test]
fn reading_seat_states_takes_no_lock_and_writes_nothing() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    let work = seat(t.path(), "work")?;
    write_seat(&work, "r-alice", "alice")?;
    let mut v = Vault::open(&vdir)?;
    v.edit(|tx| tx.capture(&work, Some("alice")))?;
    // Something to keep current: Claude Code refreshed the access token.
    write_creds(&work, "acc-new", "r-alice")?;
    let files = || -> anyhow::Result<(String, String)> {
        Ok((
            std::fs::read_to_string(vdir.join("logins.json"))?,
            std::fs::read_to_string(vdir.join("secrets.json"))?,
        ))
    };
    let before = files()?;
    // Another swarmy is changing the vault.
    let held = store::Lock::acquire(&vdir.join("lock"), std::time::Duration::ZERO)?;
    let t0 = std::time::Instant::now();
    let states = v.seat_states(std::slice::from_ref(&work))?;
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(2),
        "it waited"
    );
    assert_eq!(states[&work.id].saved_as.as_deref(), Some("alice"));
    assert_eq!(states[&work.id].account_uuid.as_deref(), Some("alice"));
    assert!(
        v.try_edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?
            .is_none(),
        "busy: skipped at once"
    );
    assert_eq!(files()?, before, "nothing was written");
    drop(held);
    assert!(
        v.try_edit(|tx| tx.seat_states(std::slice::from_ref(&work)))?
            .is_some()
    );
    let alice = login(&v, "alice")?.clone();
    assert_eq!(secret_of(&v, &alice.id)?["accessToken"], "acc-new");
    Ok(())
}

#[test]
fn multi_org_logins_stay_distinct_and_do_not_trigger_rotation() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    let def = seat(t.path(), "default")?;
    let work = seat(t.path(), "work")?;
    let write_org_seat = |s: &Seat,
                          refresh: &str,
                          org: &str,
                          sub: &str,
                          org_tier: Option<&str>|
     -> anyhow::Result<()> {
        let now = Utc::now();
        let creds = json!({
            "claudeAiOauth": {
                "accessToken": format!("acc-{refresh}"),
                "refreshToken": refresh,
                "expiresAt": (now + chrono::Duration::hours(6)).timestamp_millis(),
                "refreshTokenExpiresAt": (now + chrono::Duration::days(25)).timestamp_millis(),
                "subscriptionType": sub,
                "rateLimitTier": "default_claude_max_5x"
            }
        });
        std::fs::write(s.credentials_file(), creds.to_string())?;
        let mut oauth = json!({
            "accountUuid": "acc-same",
            "organizationUuid": org,
            "emailAddress": "dev@example.com",
            "userRateLimitTier": "default_claude_max_5x",
            "profileFetchedAt": now.timestamp_millis()
        });
        if let Some(ot) = org_tier {
            oauth["organizationRateLimitTier"] = json!(ot);
        }
        std::fs::write(&s.claude_json, json!({"oauthAccount": oauth}).to_string())?;
        Ok(())
    };
    write_org_seat(&def, "r-team", "org-team", "team", Some("default_raven"))?;
    write_org_seat(&work, "r-max", "org-max", "max", None)?;

    let mut v = Vault::open(&vdir)?;
    v.edit(|tx| tx.capture(&def, Some("dev-team")))?;
    v.edit(|tx| tx.capture(&work, Some("dev-max")))?;
    let l_team = login(&v, "dev-team")?.clone();
    let l_max = login(&v, "dev-max")?.clone();
    assert_eq!(l_team.account_uuid.as_deref(), Some("acc-same/org-team"));
    assert_eq!(l_max.account_uuid.as_deref(), Some("acc-same/org-max"));
    assert_eq!(l_team.plan.as_deref(), Some("default_raven"));
    assert_eq!(l_max.plan.as_deref(), Some("default_claude_max_5x"));

    // Logging into org-max inside the `default` seat (same accountUuid, different org, fresh token)
    // must NOT be treated as a token rotation of `dev-team`.
    write_org_seat(
        &def,
        "r-other-org",
        "org-other",
        "team",
        Some("default_raven"),
    )?;
    v.edit(|tx| tx.sync(std::slice::from_ref(&def)))?;
    let l_team_after = login(&v, "dev-team")?;
    assert_eq!(
        l_team_after.meta.fingerprint, l_team.meta.fingerprint,
        "dev-team was not overwritten by another organization's grant"
    );
    assert_eq!(l_team_after.mounted, None, "dev-team unmounted cleanly");
    Ok(())
}

#[test]
fn v2_logins_upgrade_bare_account_uuid_and_plan_from_oauth_account() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let vdir = t.path().join("vault");
    std::fs::create_dir_all(&vdir)?;
    let now = Utc::now().to_rfc3339();
    let v2 = json!({
        "v": 2,
        "logins": [{
            "id": "l0123456789",
            "label": "team-login",
            "account_uuid": "acc-1",
            "email": "dev@example.com",
            "plan": "default_claude_max_5x",
            "meta": {
                "fingerprint": "0123456789abcdef",
                "subscription_type": "team",
                "rate_limit_tier": "default_claude_max_5x",
                "access_expires_at": null,
                "refresh_expires_at": null,
                "scopes": []
            },
            "oauth_account": {
                "accountUuid": "acc-1",
                "organizationUuid": "org-team",
                "organizationRateLimitTier": "default_raven",
                "userRateLimitTier": "default_claude_max_5x"
            },
            "captured_at": now,
            "updated_at": now,
            "mounted": null
        }],
        "seats": {}
    });
    std::fs::write(vdir.join("logins.json"), v2.to_string())?;
    let v = Vault::open(&vdir)?;
    let l = login(&v, "team-login")?;
    assert_eq!(l.account_uuid.as_deref(), Some("acc-1/org-team"));
    assert_eq!(l.plan.as_deref(), Some("default_raven"));
    Ok(())
}
