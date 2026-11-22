//! `agentmana accounts`: the login vault.

use std::path::PathBuf;

use anyhow::{Context, bail};
use chrono::Local;
use clap::{Args, Subcommand};

use crate::claude::credentials;
use crate::claude::registry;
use crate::claude::seats::Seat;
use crate::core::limits::WindowKind;
use crate::ui::fmt;
use crate::ui::style::Tone;
use crate::ui::table::{Align, Cell, Table};
use crate::vault::{self, Vault};

use super::context::Ctx;

#[derive(Args, Debug, Clone)]
pub struct AccountsArgs {
    #[command(subcommand)]
    pub cmd: Option<AccountsCmd>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AccountsCmd {
    /// Seats, saved logins, and how much limit each has left.
    List,
    /// Save the login a seat uses now.
    Capture {
        seat: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Import a backup holding `{"claudeAiOauth": {...}}` (or a whole credentials file).
    Import {
        file: PathBuf,
        #[arg(long)]
        label: String,
    },
    /// Put a saved login into a seat (the seat's current login is saved first).
    Switch {
        seat: String,
        login: String,
        /// Switch even though Claude Code is running in that seat.
        #[arg(long)]
        force: bool,
        /// Allow the same login in two seats (one of them breaks when the token rotates).
        #[arg(long)]
        share: bool,
    },
    /// Log into a new account in a scratch seat (runs `claude`; use /login, then /exit) and save it.
    Add {
        #[arg(long)]
        label: String,
    },
    Rename {
        login: String,
        new_label: String,
    },
    /// Forget a saved login.
    Rm {
        login: String,
        #[arg(long)]
        force: bool,
    },
}

fn seat<'a>(ctx: &'a Ctx, key: &str) -> anyhow::Result<&'a Seat> {
    ctx.seats
        .iter()
        .find(|s| s.label == key || s.dir.to_string_lossy() == key)
        .with_context(|| {
            format!(
                "no seat '{key}'; seats: {}",
                ctx.seats
                    .iter()
                    .map(|s| s.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Claude Code processes using this seat right now.
fn live_in(ctx: &Ctx, s: &Seat) -> usize {
    let registered = registry::live_sessions(&s.label, &s.sessions_dir(), &ctx.procs).len();
    let by_env = ctx
        .procs
        .claude
        .iter()
        .filter(|p| match &p.config_dir {
            Some(d) => std::fs::canonicalize(d)
                .map(|d| d == s.dir)
                .unwrap_or(false),
            None => s.is_default,
        })
        .count();
    registered.max(by_env)
}

/// Last known weekly utilization of an account, accounting for resets since the reading.
fn weekly(ctx: &Ctx, account: Option<&str>) -> Option<(f64, chrono::DateTime<chrono::Utc>, bool)> {
    let account = account?;
    let (_, rs) = ctx
        .latest_readings()
        .into_iter()
        .find(|(a, _)| a == account)?;
    let r = rs.into_iter().find(|r| r.kind == WindowKind::SevenDay)?;
    if r.is_reset(ctx.now) {
        let mut end = r.resets_at;
        while end <= ctx.now {
            end += chrono::Duration::days(7);
        }
        Some((0.0, end, true))
    } else {
        Some((r.utilization, r.resets_at, false))
    }
}

pub fn run(ctx: &Ctx, a: &AccountsArgs) -> anyhow::Result<()> {
    let mut v = Vault::open(&ctx.dirs.vault_dir())?;
    v.sync(&ctx.seats)?;
    let p = &ctx.painter;
    match a.cmd.clone().unwrap_or(AccountsCmd::List) {
        AccountsCmd::List => list(ctx, &v),
        AccountsCmd::Capture { seat: key, label } => {
            let s = seat(ctx, &key)?;
            match v.capture(s, label.as_deref())? {
                Some(id) => {
                    let l = v.logins().iter().find(|l| l.id == id).unwrap();
                    println!(
                        "{} saved the login of seat '{}' as '{}' ({})",
                        p.paint("[ OK ]", Tone::Good),
                        s.label,
                        l.label,
                        l.meta.fingerprint
                    );
                }
                None => println!(
                    "{} seat '{}' has no login",
                    p.paint("[WARN]", Tone::Warn),
                    s.label
                ),
            }
            Ok(())
        }
        AccountsCmd::Import { file, label } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            let secret =
                credentials::parse_login(&text)?.context("no claudeAiOauth object in that file")?;
            v.put(&secret, None, None, Some(&label), None)?;
            println!(
                "{} imported '{label}' ({})",
                p.paint("[ OK ]", Tone::Good),
                secret.meta().fingerprint
            );
            println!(
                "{}",
                p.paint(
                    "  The account behind it is learnt the first time it is switched into a seat.",
                    Tone::Dim
                )
            );
            Ok(())
        }
        AccountsCmd::Switch {
            seat: key,
            login,
            force,
            share,
        } => {
            let s = seat(ctx, &key)?;
            let live = live_in(ctx, s);
            if live > 0 && !force {
                bail!(
                    "Claude Code is running in seat '{}' ({live} process(es)). Running sessions re-read the credentials file when it changes \
                     and continue under the new login, but their account display stays stale until restart. \
                     Close them first, or pass --force.",
                    s.label
                );
            }
            let rep = v.switch(s, &login, share)?;
            if rep.unchanged {
                println!(
                    "{} seat '{}' already uses '{}'",
                    p.paint("[ OK ]", Tone::Good),
                    rep.seat,
                    rep.to
                );
                return Ok(());
            }
            println!(
                "{} seat '{}': {} → {}",
                p.paint("[ OK ]", Tone::Good),
                rep.seat,
                rep.from.as_deref().unwrap_or("(nothing)"),
                p.paint(&rep.to, Tone::Bold)
            );
            if let Some(b) = rep.backup {
                println!(
                    "{}",
                    p.paint(
                        &format!("  backups in {}", ctx.show_path(&b.to_string_lossy())),
                        Tone::Dim
                    )
                );
            }
            if live > 0 {
                println!("{}", p.paint("  running sessions pick up the new login on their next request; restart them to refresh the account shown", Tone::Warn));
            }
            Ok(())
        }
        AccountsCmd::Add { label } => {
            let scratch = ctx
                .dirs
                .vault_dir()
                .join(format!("new-{}", chrono::Utc::now().format("%Y%m%d%H%M%S")));
            std::fs::create_dir_all(&scratch)?;
            println!(
                "Starting Claude Code in a scratch seat. Run /login, choose the account, then /exit."
            );
            let status = std::process::Command::new("claude")
                .env("CLAUDE_CONFIG_DIR", &scratch)
                .status();
            let result = (|| -> anyhow::Result<()> {
                let status = status.context("cannot start `claude` (is it on PATH?)")?;
                if !status.success() {
                    eprintln!("claude exited with {status}");
                }
                let tmp = Seat {
                    label: format!("scratch-{label}"),
                    claude_json: scratch.join(".claude.json"),
                    dir: scratch.clone(),
                    is_default: false,
                    found_by: vec![],
                };
                let s = vault::read_seat(&tmp)?;
                let secret = s.secret.context("no login was made in the scratch seat")?;
                v.put(
                    &secret,
                    s.oauth_account,
                    s.account.as_ref(),
                    Some(&label),
                    None,
                )?;
                println!(
                    "{} saved '{label}' ({})",
                    p.paint("[ OK ]", Tone::Good),
                    secret.meta().fingerprint
                );
                Ok(())
            })();
            let _ = std::fs::remove_dir_all(&scratch);
            result
        }
        AccountsCmd::Rename { login, new_label } => {
            v.rename(&login, &new_label)?;
            println!("{} renamed to '{new_label}'", p.paint("[ OK ]", Tone::Good));
            Ok(())
        }
        AccountsCmd::Rm { login, force } => {
            let l = v.remove(&login, force)?;
            println!("{} forgot '{}'", p.paint("[ OK ]", Tone::Good), l.label);
            Ok(())
        }
    }
}

fn list(ctx: &Ctx, v: &Vault) -> anyhow::Result<()> {
    let p = &ctx.painter;
    println!(
        "{} {}",
        p.paint("SEATS", Tone::Header),
        p.paint(
            "· Claude Code config dirs and the login each uses",
            Tone::Dim
        )
    );
    let mut t = Table::new(&[
        ("SEAT", Align::Left),
        ("LOGIN", Align::Left),
        ("ACCOUNT", Align::Left),
        ("WEEK", Align::Left),
        ("RESETS", Align::Left),
        ("LIVE", Align::Right),
    ]);
    for (s, si) in ctx.seats.iter().zip(&ctx.seat_info) {
        let login = v
            .logins()
            .iter()
            .find(|l| l.mounted.as_deref() == Some(&s.label));
        let acct = si.account.as_ref().map(|a| a.account_uuid.clone());
        let w = weekly(ctx, acct.as_deref());
        let (week, tone, resets) = match w {
            Some((u, end, reset)) => (
                format!(
                    "{} {}{}",
                    fmt::gauge(u / 100.0, 10),
                    fmt::pct(u),
                    if reset { " (reset)" } else { "" }
                ),
                Tone::of_utilization(u),
                end.with_timezone(&Local).format("%a %H:%M").to_string(),
            ),
            None => ("-".into(), Tone::Dim, String::new()),
        };
        t.row(vec![
            Cell::tone(&s.label, Tone::Bold),
            match login {
                Some(l) => Cell::new(l.label.clone()),
                None => Cell::tone("(not saved)", Tone::Dim),
            },
            Cell::new(
                acct.as_deref()
                    .map(|a| ctx.account_name(a))
                    .unwrap_or_else(|| "(logged out)".into()),
            ),
            Cell::tone(week, tone),
            Cell::tone(resets, Tone::Dim),
            Cell::new(live_in(ctx, s).to_string()),
        ]);
    }
    println!("{}", t.render(p));
    println!();
    println!(
        "{} {}",
        p.paint("VAULT", Tone::Header),
        p.paint(
            &format!(
                "· {} · {}",
                v.store.name(),
                ctx.show_path(&ctx.dirs.vault_dir().to_string_lossy())
            ),
            Tone::Dim
        )
    );
    if v.logins().is_empty() {
        println!(
            "  {}",
            p.paint(
                "empty — save the current logins with `agentmana accounts capture <seat>`",
                Tone::Dim
            )
        );
        return Ok(());
    }
    let mut t = Table::new(&[
        ("LOGIN", Align::Left),
        ("EMAIL", Align::Left),
        ("IN SEAT", Align::Left),
        ("WEEK", Align::Left),
        ("LOGIN EXPIRES", Align::Left),
        ("GRANT", Align::Left),
    ]);
    let mut best: Option<(f64, &str)> = None;
    for l in v.logins() {
        let w = weekly(ctx, l.account_uuid.as_deref());
        let week = match w {
            Some((u, _, reset)) => {
                if l.mounted.is_none() && best.is_none_or(|(b, _)| u < b) {
                    best = Some((u, &l.label));
                }
                Cell::tone(
                    format!(
                        "{} {}{}",
                        fmt::gauge(u / 100.0, 10),
                        fmt::pct(u),
                        if reset { " (reset)" } else { "" }
                    ),
                    Tone::of_utilization(u),
                )
            }
            None => Cell::tone("unknown", Tone::Dim),
        };
        let expires = l.meta.refresh_expires_at.map(|t| {
            let left = t - ctx.now;
            let s = format!(
                "{} (in {})",
                t.with_timezone(&Local).format("%b %d"),
                fmt::duration(left)
            );
            Cell::tone(
                s,
                if left.num_days() < 7 {
                    Tone::Warn
                } else {
                    Tone::Dim
                },
            )
        });
        t.row(vec![
            Cell::tone(&l.label, Tone::Bold),
            Cell::new(if ctx.mask {
                "•••".into()
            } else {
                l.email.clone().unwrap_or_default()
            }),
            match &l.mounted {
                Some(s) => Cell::tone(s.clone(), Tone::Good),
                None => Cell::tone("-", Tone::Dim),
            },
            week,
            expires.unwrap_or_else(|| Cell::tone("-", Tone::Dim)),
            Cell::tone(
                l.meta.fingerprint[..8.min(l.meta.fingerprint.len())].to_string(),
                Tone::Dim,
            ),
        ]);
    }
    println!("{}", t.render(p));
    if let Some((u, label)) = best {
        println!();
        println!("{}", p.paint(&format!("tip: '{label}' has the most limit left ({} used): agentmana accounts switch <seat> {label}", fmt::pct(u)), Tone::Accent));
    }
    Ok(())
}
