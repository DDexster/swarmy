//! Command-line interface.

pub mod accounts;
pub mod context;
pub mod doctor;
pub mod limits;
pub mod models;
pub mod prices;
pub mod report;
pub mod setup;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::core::period::Period;
use crate::index::query::{Dim, Filter};
use crate::ui::style::{ColorChoice, Painter};

#[derive(Parser, Debug)]
#[command(
    name = "swarmy",
    version,
    about = "Mission control for Claude Code agents: live swarm, API-equivalent spend, weekly-limit attribution, login vault",
    long_about = None,
    after_help = "Run without a command for the live dashboard. Data never leaves this machine."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// When to use colours.
    #[arg(long, value_enum, global = true, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,
    /// Hide emails, names and full paths (for screenshots).
    #[arg(long, global = true)]
    pub mask: bool,
    /// Skip the boot animation of the dashboard.
    #[arg(long, global = true)]
    pub no_boot: bool,
    /// Low-power dashboard: fewer frames and refreshes (automatic on battery).
    #[arg(long, global = true)]
    pub eco: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// API-equivalent spend grouped by project, account, model, agent, day…
    Report(ReportArgs),
    /// Usage statistics per model: cost, cache hits, thinking, effort mix, substitutions.
    Models(PeriodFilterArgs),
    /// Weekly / 5-hour limits per account: where they went and when they run out.
    Limits {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
        /// Projects to list per window.
        #[arg(long, default_value_t = 8)]
        top: usize,
    },
    /// The login vault: save, switch and list OAuth logins across seats.
    Accounts(accounts::AccountsArgs),
    /// What was found on this machine and whether it is readable.
    Doctor,
    /// Optional: add the swarmy status line to Claude Code (exact limits) and save logins.
    Setup(setup::SetupArgs),
    /// Switch tmux to the Claude Code that has waited for you the longest (bind it to a key).
    Jump {
        /// Only list who waits, do not switch.
        #[arg(long)]
        list: bool,
    },
    /// Used by Claude Code's status line: records the limits it passes and prints a short line.
    Statusline {
        /// A status-line command to run first (the one you had before), with the same input.
        #[arg(long)]
        then: Option<String>,
    },
    /// The price in force for every model and its source.
    Prices {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Try the dashboard on a made-up world of agents (no real data is read).
    Demo {
        /// Record N frames headlessly into this file instead of opening the dashboard.
        #[arg(long, hide = true)]
        record: Option<std::path::PathBuf>,
        #[arg(long, hide = true, default_value_t = 100)]
        frames: usize,
        #[arg(long, hide = true, default_value = "140x40")]
        size: String,
        /// Keys to press while recording: `frame:key,frame:key` (key `enter`, `esc` or a character).
        #[arg(long, hide = true, default_value = "")]
        keys: String,
    },
    /// Print shell completions.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the man page (roff).
    Man,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
    Csv,
}

#[derive(Args, Debug, Clone)]
pub struct FilterArgs {
    /// Only this account (uuid or email).
    #[arg(long)]
    pub account: Option<String>,
    /// Only this seat (config dir label).
    #[arg(long)]
    pub seat: Option<String>,
    /// Only this project (name or path).
    #[arg(long)]
    pub project: Option<String>,
    /// Only models whose id contains this.
    #[arg(long)]
    pub model: Option<String>,
}

impl FilterArgs {
    pub fn to_filter(&self) -> Filter {
        Filter {
            account: self.account.clone(),
            seat: self.seat.clone(),
            project: self.project.clone(),
            model: self.model.clone(),
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct PeriodFilterArgs {
    /// today, yesterday, week (current limit window), month, all, 24h, 7d, YYYY-MM-DD, FROM..TO
    #[arg(short, long, default_value = "week")]
    pub period: Period,
    #[command(flatten)]
    pub filter: FilterArgs,
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,
}

#[derive(Args, Debug, Clone)]
pub struct ReportArgs {
    /// Grouping.
    #[arg(short, long, value_enum, default_value_t = Dim::Project)]
    pub by: Dim,
    /// today, yesterday, week (current limit window), month, all, 24h, 7d, YYYY-MM-DD, FROM..TO
    #[arg(short, long, default_value = "week")]
    pub period: Period,
    #[command(flatten)]
    pub filter: FilterArgs,
    /// Rows to show (time groupings show all).
    #[arg(long, default_value_t = 15)]
    pub top: usize,
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,
}

pub fn run(cli: Cli) -> anyhow::Result<()> {
    let painter = Painter::new(cli.color);
    match cli.command {
        Some(Command::Completions { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "swarmy", &mut std::io::stdout());
            Ok(())
        }
        Some(Command::Jump { list }) => jump(list),
        Some(Command::Statusline { then }) => {
            // Runs after every model response: no index, no scanning, just record and print.
            let dirs = crate::config::Dirs::resolve()?;
            let config = crate::config::Config::load(&dirs).unwrap_or_default();
            crate::claude::statusline::run(&dirs, &config.statusline, then.as_deref())
        }
        Some(Command::Man) => {
            clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
            Ok(())
        }
        Some(Command::Demo {
            record,
            frames,
            size,
            keys,
        }) => {
            let dirs = crate::config::Dirs::resolve()?;
            let demo = crate::demo::start(&dirs.data.join("demo"))?;
            let mut ctx = context::Ctx::demo(painter, demo.seats.clone(), &demo.root)?;
            // Saved logins, so the VAULT screen has something to show.
            {
                let mut v = crate::vault::Vault::open(&ctx.dirs.vault_dir())?;
                for s in &ctx.seats {
                    v.capture(s, Some(&format!("alex-{}", s.label)))?;
                }
                let spare = crate::claude::credentials::Secret::new(serde_json::json!({
                    "accessToken": "demo-access-spare", "refreshToken": "demo-refresh-spare",
                    "refreshTokenExpiresAt": (chrono::Utc::now() + chrono::Duration::days(4)).timestamp_millis()
                }));
                let acct = crate::claude::config_json::Account {
                    account_uuid: "acc-demo-spare".into(),
                    email: Some("alex+spare@example.com".into()),
                    ..Default::default()
                };
                v.put(&spare, Some(serde_json::json!({"accountUuid": "acc-demo-spare", "emailAddress": "alex+spare@example.com"})), Some(&acct), Some("alex-spare"), None)?;
            }
            // Account names come from the saved logins just made.
            ctx.drop_unconfirmed();
            match record {
                Some(out) => {
                    let (w, h) = size
                        .split_once('x')
                        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
                        .unwrap_or((140, 40));
                    let keys: Vec<(usize, char)> = keys
                        .split(',')
                        .filter_map(|p| {
                            let (f, k) = p.split_once(':')?;
                            let c = match k {
                                "enter" => '\n',
                                "esc" => '\x1b',
                                k => k.chars().next()?,
                            };
                            Some((f.parse().ok()?, c))
                        })
                        .collect();
                    crate::tui::record(ctx, &out, w, h, frames, &keys)
                }
                None => crate::tui::run(ctx, cli.no_boot, cli.eco),
            }
        }
        command => {
            let ctx = context::Ctx::load(painter, cli.mask)?;
            match command {
                Some(Command::Report(a)) => report::run(&ctx, &a),
                Some(Command::Models(a)) => models::run(&ctx, &a),
                Some(Command::Limits { format, top }) => limits::run(&ctx, format, top),
                Some(Command::Accounts(a)) => accounts::run(&ctx, &a),
                Some(Command::Doctor) => doctor::run(&ctx),
                Some(Command::Prices { format }) => prices::run(&ctx, format),
                None => {
                    use std::io::IsTerminal;
                    if std::io::stdout().is_terminal() {
                        crate::tui::run(ctx, cli.no_boot, cli.eco)
                    } else {
                        limits::run(&ctx, Format::Table, 8)
                    }
                }
                Some(Command::Setup(a)) => setup::run(&ctx, &a),
                Some(
                    Command::Completions { .. }
                    | Command::Man
                    | Command::Demo { .. }
                    | Command::Statusline { .. }
                    | Command::Jump { .. },
                ) => {
                    unreachable!()
                }
            }
        }
    }
}

/// `swarmy jump`: the oldest session that waits for the user, from the dashboard's summary when
/// it runs, else straight from the session registries.
fn jump(list: bool) -> anyhow::Result<()> {
    let dirs = crate::config::Dirs::resolve()?;
    let config = crate::config::Config::load(&dirs).unwrap_or_default();
    let now = chrono::Utc::now();
    // (name, what, tmux)
    let waiting: Vec<(String, String, Option<String>)> = match crate::fleet::read_fresh(
        &crate::fleet::path(&dirs.data),
        config.statusline.fresh_seconds,
        now,
    ) {
        Some(f) => f
            .needs_you
            .into_iter()
            .map(|n| (n.name, n.what, n.tmux))
            .collect(),
        None => {
            let mut seat_dirs = vec![dirs.home.join(".claude")];
            if let Ok(rd) = std::fs::read_dir(&dirs.home) {
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().starts_with(".claude-") {
                        seat_dirs.push(e.path());
                    }
                }
            }
            seat_dirs
                .iter()
                .flat_map(|d| crate::claude::registry::waiting_sessions(&d.join("sessions")))
                .map(|(_, name, what, tmux, cwd)| {
                    (
                        name.unwrap_or_else(|| cwd.rsplit('/').next().unwrap_or("").to_string()),
                        what,
                        tmux,
                    )
                })
                .collect()
        }
    };
    if waiting.is_empty() {
        println!("nothing waits for you");
        return Ok(());
    }
    if list {
        for (name, what, tmux) in &waiting {
            println!(
                "▲ {name}: {what}{}",
                tmux.as_ref()
                    .map(|t| format!("  (tmux {t})"))
                    .unwrap_or_default()
            );
        }
        return Ok(());
    }
    let Some((name, what, Some(pane))) = waiting.iter().find(|w| w.2.is_some()).cloned() else {
        anyhow::bail!(
            "{} session(s) wait for you, none inside tmux",
            waiting.len()
        );
    };
    match crate::claude::tmux::switch_to(&pane) {
        crate::claude::tmux::Jump::Done => {
            println!("→ {name}: {what}");
            Ok(())
        }
        crate::claude::tmux::Jump::NotInTmux { hint } => {
            anyhow::bail!("not inside tmux; attach with: {hint}")
        }
        crate::claude::tmux::Jump::Failed => anyhow::bail!("tmux could not switch to {pane}"),
    }
}
