//! Claude Code's status line as a source of exact limit readings.
//!
//! When `settings.json` has `"statusLine": {"type": "command", "command": "swarmy statusline"}`,
//! Claude Code runs the command after each model response and passes a JSON on stdin. For
//! subscribers it carries `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}`, taken
//! from the response itself: the real numbers, no extra request. `swarmy statusline` writes them
//! to `<data>/statusline/<seat key>.json` and prints a short line for Claude Code to show. The
//! dashboard reads those files as limit readings of the seat's account.
//!
//! Format (Claude Code 2.1.329, documented in its binary): `rate_limits` is present only for
//! subscribers, after the first API response, while a window is active; `resets_at` is in epoch
//! seconds.

use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::claude::config_json::Utilization;
use crate::core::ids::SeatId;
use crate::core::limits::{Level, Reading, WindowKind};

/// What `swarmy statusline` keeps of one call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveLimits {
    /// When Claude Code ran the status line.
    pub at: DateTime<Utc>,
    /// The config dir of the Claude Code that ran it.
    pub config_dir: String,
    pub session_id: Option<String>,
    /// (used %, resets at) per window.
    pub five_hour: Option<(f64, DateTime<Utc>)>,
    pub seven_day: Option<(f64, DateTime<Utc>)>,
}

/// The part of Claude Code's status-line input that swarmy uses.
#[derive(Debug, Default, Deserialize)]
pub struct Input {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub model: Option<Model>,
    #[serde(default)]
    pub cost: Option<Cost>,
    #[serde(default)]
    pub context_window: Option<ContextWindow>,
    #[serde(default)]
    pub rate_limits: Option<RateLimits>,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub prompt_cache: Option<PromptCache>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Effort {
    #[serde(default)]
    pub level: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Workspace {
    #[serde(default)]
    pub current_dir: Option<String>,
    #[serde(default)]
    pub git_worktree: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PromptCache {
    #[serde(default)]
    pub warm: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Model {
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Cost {
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ContextWindow {
    #[serde(default)]
    pub used_percentage: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RateLimits {
    #[serde(default)]
    pub five_hour: Option<Window>,
    #[serde(default)]
    pub seven_day: Option<Window>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Window {
    pub used_percentage: f64,
    /// Epoch seconds.
    pub resets_at: f64,
}

impl Window {
    fn pair(&self) -> Option<(f64, DateTime<Utc>)> {
        let t = Utc.timestamp_opt(self.resets_at as i64, 0).single()?;
        Some((self.used_percentage, t))
    }
}

/// The file a seat's readings go to: named by its seat id (a hash of its config dir), so
/// `swarmy statusline` needs nothing but `CLAUDE_CONFIG_DIR` to find it.
pub fn file_for(data_dir: &Path, config_dir: &Path) -> PathBuf {
    file_of(data_dir, &crate::claude::seats::seat_id(config_dir))
}

/// The status-line file of a seat id.
pub fn file_of(data_dir: &Path, seat: &SeatId) -> PathBuf {
    data_dir.join("statusline").join(format!("{seat}.json"))
}

/// The config dir of the Claude Code that runs us: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn current_config_dir(home: &Path) -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"))
}

pub fn parse_input(text: &str) -> Input {
    serde_json::from_str(text).unwrap_or_default()
}

pub fn live_from(input: &Input, config_dir: &Path, at: DateTime<Utc>) -> Option<LiveLimits> {
    let rl = input.rate_limits.as_ref()?;
    let five_hour = rl.five_hour.as_ref().and_then(Window::pair);
    let seven_day = rl.seven_day.as_ref().and_then(Window::pair);
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(LiveLimits {
        at,
        config_dir: config_dir.to_string_lossy().into_owned(),
        session_id: input.session_id.clone(),
        five_hour,
        seven_day,
    })
}

/// Writes atomically (`fsutil::atomic_write`); errors are ignored by the caller, a status line
/// must never fail.
pub fn write(path: &Path, live: &LiveLimits) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(live)?;
    crate::fsutil::atomic_write(path, &bytes, crate::fsutil::Mode::Private)
}

pub fn read(path: &Path) -> Option<LiveLimits> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The seat's utilization: the status-line reading when it is newer than Claude Code's cached
/// one, for the same account.
pub fn merge(
    cached: Option<Utilization>,
    account: Option<&str>,
    live: Option<LiveLimits>,
) -> Option<Utilization> {
    let (Some(live), Some(account)) = (live, account) else {
        return cached;
    };
    if cached.as_ref().is_some_and(|c| c.fetched_at >= live.at) {
        return cached;
    }
    let mut readings: Vec<Reading> = Vec::new();
    for (kind, v) in [
        (WindowKind::FiveHour, live.five_hour),
        (WindowKind::SevenDay, live.seven_day),
    ] {
        if let Some((utilization, resets_at)) = v {
            readings.push(Reading {
                kind,
                utilization,
                resets_at,
                fetched_at: live.at,
            });
        }
    }
    // Windows the status line does not carry (per-model weekly limits) keep the cached values.
    if let Some(c) = cached.as_ref().filter(|c| c.account_uuid == account) {
        readings.extend(
            c.readings
                .iter()
                .filter(|r| !matches!(r.kind, WindowKind::FiveHour | WindowKind::SevenDay))
                .copied(),
        );
    }
    Some(Utilization {
        account_uuid: account.to_string(),
        fetched_at: live.at,
        readings,
        extra_usage_enabled: cached.map(|c| c.extra_usage_enabled).unwrap_or(false),
    })
}

/// One piece of the line: its forms from full to shortest (text with colour, plain width).
struct Segment {
    key: &'static str,
    forms: Vec<(String, usize)>,
    form: usize,
    dropped: bool,
}

/// Everything the line is made from besides stdin.
pub struct Extra {
    pub fleet: Option<crate::fleet::Fleet>,
    /// Sessions waiting for the user when there is no fresh fleet summary:
    /// (session id, name, what, tmux).
    pub waiting: Vec<(String, String, String, Option<String>)>,
    /// This seat's key in the fleet summary.
    pub seat_key: SeatId,
    pub mask: bool,
    pub color: bool,
    pub segments: Vec<String>,
    pub width: usize,
    pub lines: u8,
}

struct Paint(bool);

impl Paint {
    fn c(&self, s: &str, code: &str) -> String {
        if self.0 {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

fn form(p: &Paint, parts: &[(&str, &str)]) -> (String, usize) {
    let text: String = parts
        .iter()
        .map(|(t, c)| {
            if c.is_empty() {
                t.to_string()
            } else {
                p.c(t, c)
            }
        })
        .collect();
    let width = parts
        .iter()
        .map(|(t, _)| unicode_width::UnicodeWidthStr::width(*t))
        .sum();
    (text, width)
}

/// A context window this full turns amber, then red (not a limit: its own thresholds).
const CONTEXT_WARN: f64 = 70.0;
const CONTEXT_BAD: f64 = 85.0;

/// The ANSI code of a limit's level (`core::limits::Level`), the same colours as the dashboard.
fn level(pct: f64) -> &'static str {
    match Level::of(pct) {
        Level::Ok => "32",
        Level::Warn => "33",
        Level::Bad | Level::Reached => "1;31",
    }
}

fn hm(d: chrono::Duration) -> String {
    let m = d.num_minutes().max(0);
    if m >= 24 * 60 {
        format!("{}d {}h", m / 1440, (m % 1440) / 60)
    } else if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

/// The current git branch of `dir`, read from `.git/HEAD` (never by running git).
fn branch_of(dir: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let git = d.join(".git");
        let head = if git.is_dir() {
            git.join("HEAD")
        } else if git.is_file() {
            let t = std::fs::read_to_string(&git).ok()?;
            let gd = t
                .lines()
                .find_map(|l| l.strip_prefix("gitdir:"))?
                .trim()
                .to_string();
            let gd = if Path::new(&gd).is_absolute() {
                PathBuf::from(gd)
            } else {
                d.join(gd)
            };
            gd.join("HEAD")
        } else {
            cur = d.parent();
            continue;
        };
        let h = std::fs::read_to_string(head).ok()?;
        return Some(match h.trim().strip_prefix("ref: refs/heads/") {
            Some(b) => b.to_string(),
            None => h.trim().chars().take(8).collect(),
        });
    }
    None
}

/// The status line: who needs you, this session's agents, the week, 5 hours, context, cost and
/// burn; shortened to `x.width` in a fixed order; optionally a second line.
pub fn render(input: &Input, x: &Extra, now: DateTime<Utc>) -> String {
    let p = Paint(x.color);
    let me = input.session_id.clone().unwrap_or_default();
    let seat = x.fleet.as_ref().and_then(|f| f.seats.get(&x.seat_key));
    let mut segs: Vec<Segment> = Vec::new();
    for key in &x.segments {
        let forms: Vec<(String, usize)> = match key.as_str() {
            "attention" => {
                let others: Vec<(String, String, Option<String>)> = match &x.fleet {
                    Some(f) => f
                        .needs_you
                        .iter()
                        .filter(|n| n.session_id.as_str() != me)
                        .map(|n| (n.name.clone(), n.what.clone(), n.tmux.clone()))
                        .collect(),
                    None => x
                        .waiting
                        .iter()
                        .filter(|w| w.0 != me)
                        .map(|w| (w.1.clone(), w.2.clone(), w.3.clone()))
                        .collect(),
                };
                if others.is_empty() {
                    continue;
                }
                let n = others.len();
                let names: Vec<String> = others
                    .iter()
                    .map(|o| {
                        if x.mask {
                            "•••".into()
                        } else {
                            o.0.clone()
                        }
                    })
                    .collect();
                let full = if n == 1 {
                    format!("▲ {} needs you: {}", names[0], others[0].1)
                } else {
                    format!("▲ {n} need you: {}", names.join(", "))
                };
                let mut v = Vec::new();
                if !x.mask {
                    v.push(form(&p, &[(&full, "1;33"), (" · swarmy n J", "2")]));
                }
                v.push(form(
                    &p,
                    &[
                        (
                            &format!("▲ {n} need{} you", if n == 1 { "s" } else { "" }),
                            "1;33",
                        ),
                        (" · n J", "2"),
                    ],
                ));
                v.push(form(&p, &[(&format!("▲{n}"), "1;33")]));
                v
            }
            "agents" => {
                let Some(sc) = x
                    .fleet
                    .as_ref()
                    .and_then(|f| f.sessions.get(me.as_str()))
                    .filter(|s| s.agents > 0)
                else {
                    continue;
                };
                let mut parts: Vec<(String, &str)> = vec![(
                    format!(
                        "◐ {} agent{} ",
                        sc.agents,
                        if sc.agents == 1 { "" } else { "s" }
                    ),
                    "36",
                )];
                if sc.working > 0 {
                    parts.push((format!("●{} ", sc.working), "32"));
                }
                if sc.thinking > 0 {
                    parts.push((format!("◌{} ", sc.thinking), "36"));
                }
                if sc.waiting > 0 {
                    parts.push((format!("◐{} ", sc.waiting), "36"));
                }
                if sc.stalled > 0 {
                    parts.push((format!("✖{} ", sc.stalled), "1;31"));
                }
                let last = parts.len() - 1;
                parts[last].0 = parts[last].0.trim_end().to_string();
                let refs: Vec<(&str, &str)> = parts.iter().map(|(t, c)| (t.as_str(), *c)).collect();
                vec![
                    form(&p, &refs),
                    form(&p, &[(&format!("◐{}", sc.agents), "36")]),
                ]
            }
            "week" => {
                // Exact from stdin; else the dashboard's estimate (≈).
                let (pct, resets, exact) = match input
                    .rate_limits
                    .as_ref()
                    .and_then(|r| r.seven_day.as_ref())
                    .and_then(Window::pair)
                {
                    Some((p, t)) => (p, Some(t), true),
                    None => match seat.and_then(|s| s.week_used.map(|w| (w, s.resets_at))) {
                        Some((p, t)) => (p, t, false),
                        None => continue,
                    },
                };
                let approx = if exact { "" } else { "≈" };
                let head = format!("week used {approx}{pct:.0}%");
                let code = level(pct);
                let when = |t: DateTime<Utc>| {
                    t.with_timezone(&chrono::Local)
                        .format("%a %H:%M")
                        .to_string()
                };
                if Level::of(pct) == Level::Reached {
                    let r = resets
                        .map(|t| format!(" · resets {}", when(t)))
                        .unwrap_or_default();
                    vec![
                        form(
                            &p,
                            &[(&head, "1;31"), (" · limit reached", "1;31"), (&r, "2")],
                        ),
                        form(&p, &[(&head, "1;31"), (" · limit reached", "1;31")]),
                        form(&p, &[(&format!("week {approx}{pct:.0}%"), "1;31")]),
                    ]
                } else if let Some(out) = seat.and_then(|s| s.runs_out_at).filter(|t| *t > now) {
                    let before_reset = resets.is_none_or(|r| out < r);
                    let code = if before_reset && out - now < chrono::Duration::hours(24) {
                        "1;31"
                    } else {
                        code
                    };
                    vec![
                        form(
                            &p,
                            &[
                                (&head, code),
                                (
                                    &format!(" · runs out in {} ({})", hm(out - now), when(out)),
                                    code,
                                ),
                            ],
                        ),
                        form(
                            &p,
                            &[
                                (&head, code),
                                (&format!(" · runs out in {}", hm(out - now)), code),
                            ],
                        ),
                        form(&p, &[(&format!("week {approx}{pct:.0}%"), code)]),
                    ]
                } else {
                    let r = resets
                        .map(|t| format!(" · resets {}", when(t)))
                        .unwrap_or_default();
                    vec![
                        form(&p, &[(&head, code), (&r, "2")]),
                        form(&p, &[(&head, code)]),
                        form(&p, &[(&format!("week {approx}{pct:.0}%"), code)]),
                    ]
                }
            }
            "five_hour" => {
                let Some((pct, _)) = input
                    .rate_limits
                    .as_ref()
                    .and_then(|r| r.five_hour.as_ref())
                    .and_then(Window::pair)
                else {
                    continue;
                };
                vec![form(&p, &[(&format!("5h {pct:.0}%"), level(pct))])]
            }
            "context" => {
                let Some(c) = input
                    .context_window
                    .as_ref()
                    .and_then(|c| c.used_percentage)
                else {
                    continue;
                };
                let code = if c >= CONTEXT_BAD {
                    "1;31"
                } else if c >= CONTEXT_WARN {
                    "33"
                } else {
                    ""
                };
                vec![form(&p, &[(&format!("ctx {c:.0}%"), code)])]
            }
            "cost" => {
                let Some(c) = input.cost.as_ref().and_then(|c| c.total_cost_usd) else {
                    continue;
                };
                vec![form(&p, &[(&format!("${c:.2}"), "")])]
            }
            "burn" => {
                let Some(b) = x
                    .fleet
                    .as_ref()
                    .and_then(|f| f.sessions.get(me.as_str()))
                    .map(|s| s.burn_usd_h)
                    .filter(|b| *b > 0.0)
                else {
                    continue;
                };
                vec![form(&p, &[(&format!("${b:.1}/h"), "2")])]
            }
            _ => continue,
        };
        segs.push(Segment {
            key: static_key(key),
            forms,
            form: 0,
            dropped: false,
        });
    }
    // Shorten until it fits: drop or shorten pieces, least useful first; the alert and the week
    // never go.
    let sep = 3usize;
    let width = |segs: &[Segment]| -> usize {
        let live: Vec<&Segment> = segs.iter().filter(|s| !s.dropped).collect();
        live.iter().map(|s| s.forms[s.form].1).sum::<usize>() + sep * live.len().saturating_sub(1)
    };
    let pct_of = |k: &str| -> f64 {
        match k {
            "five_hour" => input
                .rate_limits
                .as_ref()
                .and_then(|r| r.five_hour.as_ref())
                .map(|w| w.used_percentage)
                .unwrap_or(0.0),
            "context" => input
                .context_window
                .as_ref()
                .and_then(|c| c.used_percentage)
                .unwrap_or(0.0),
            _ => 0.0,
        }
    };
    let steps: [(&str, bool); 10] = [
        ("burn", true),
        ("cost", true),
        ("five_hour", true),
        ("week", false),
        ("agents", false),
        ("attention", false),
        ("context", true),
        ("week", false),
        ("attention", false),
        ("agents", true),
    ];
    for (key, drop) in steps {
        if width(&segs) <= x.width {
            break;
        }
        if let Some(sg) = segs.iter_mut().find(|s| s.key == key && !s.dropped) {
            if drop {
                // A 5-hour limit from amber on and a context past 70% stay.
                let keep = (key == "five_hour" && Level::of(pct_of(key)) >= Level::Warn)
                    || (key == "context" && pct_of(key) >= CONTEXT_WARN);
                if !keep {
                    sg.dropped = true;
                }
            } else if sg.form + 1 < sg.forms.len() {
                sg.form += 1;
            }
        }
    }
    let line: Vec<String> = segs
        .iter()
        .filter(|s| !s.dropped)
        .map(|s| s.forms[s.form].0.clone())
        .collect();
    let mut out = if line.is_empty() {
        p.c("swarmy", "2")
    } else {
        line.join(&p.c(" │ ", "2"))
    };
    if x.lines >= 2 {
        let mut second: Vec<String> = Vec::new();
        if let Some(m) = input.model.as_ref().and_then(|m| m.display_name.clone()) {
            let e = input
                .effort
                .as_ref()
                .and_then(|e| e.level.clone())
                .map(|e| format!(" {e}"))
                .unwrap_or_default();
            second.push(format!("{m}{e}"));
        }
        if let Some(dir) = input.workspace.as_ref().and_then(|w| w.current_dir.clone())
            && let Some(b) = branch_of(Path::new(&dir))
        {
            let wt = input
                .workspace
                .as_ref()
                .and_then(|w| w.git_worktree.as_ref())
                .is_some_and(|v| !v.is_null());
            second.push(format!("⎇ {b}{}", if wt { " (worktree)" } else { "" }));
        }
        if let Some(n) = input.session_name.clone().filter(|_| !x.mask) {
            second.push(n);
        }
        if input.prompt_cache.as_ref().and_then(|c| c.warm) == Some(false) {
            second.push("cache cold".into());
        }
        if !second.is_empty() {
            let l = second.join(" · ");
            let l: String = l.chars().take(x.width).collect();
            out.push('\n');
            out.push_str(&p.c(&l, "2"));
        }
    }
    out
}

fn static_key(k: &str) -> &'static str {
    [
        "attention",
        "agents",
        "week",
        "five_hour",
        "context",
        "cost",
        "burn",
    ]
    .into_iter()
    .find(|s| *s == k)
    .unwrap_or("?")
}

/// `swarmy statusline [--then CMD]`: record, then print (after the user's own status line).
pub fn run(
    dirs: &crate::config::Dirs,
    conf: &crate::config::StatuslineConf,
    then: Option<&str>,
) -> anyhow::Result<()> {
    let mut text = String::new();
    let _ = std::io::stdin().read_to_string(&mut text);
    let input = parse_input(&text);
    let dir = current_config_dir(&dirs.home);
    let now = Utc::now();
    let file = file_for(&dirs.data, &dir);
    if let Some(live) = live_from(&input, &dir, now) {
        // Only when the numbers moved: the file is the dashboard's trigger.
        let old = read(&file);
        if old.as_ref().is_none_or(|o| {
            o.five_hour != live.five_hour
                || o.seven_day != live.seven_day
                || (now - o.at).num_minutes() >= 5
        }) && let Err(e) = write(&file, &live)
        {
            crate::diag::log(
                "statusline",
                &format!("cannot write {}: {e}", file.display()),
            );
        }
    }
    let fleet = crate::fleet::read_fresh(&crate::fleet::path(&dirs.data), conf.fresh_seconds, now);
    // No dashboard running: find the sessions that wait for the user ourselves.
    let waiting = if fleet.is_some() || !conf.segments.iter().any(|s| s == "attention") {
        Vec::new()
    } else {
        let mut dirs_: Vec<PathBuf> = vec![dirs.home.join(".claude"), dir.clone()];
        if let Ok(rd) = std::fs::read_dir(&dirs.home) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(".claude-")
                    && e.path().join("sessions").is_dir()
                {
                    dirs_.push(e.path());
                }
            }
        }
        dirs_.sort();
        dirs_.dedup();
        dirs_
            .iter()
            .flat_map(|d| crate::claude::registry::waiting_sessions(&d.join("sessions")))
            .map(|w| {
                (
                    w.session_id.to_string(),
                    w.label(),
                    w.what.clone(),
                    w.tmux.clone(),
                )
            })
            .collect()
    };
    let seat_key = crate::claude::seats::seat_id(&dir);
    let columns: usize = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(120);
    let theirs = then
        .and_then(|cmd| run_previous(cmd, &text))
        .filter(|s| !s.trim().is_empty());
    let theirs_w = theirs
        .as_ref()
        .map(|t| unicode_width::UnicodeWidthStr::width(t.trim_end()) + 3)
        .unwrap_or(0);
    let x = Extra {
        fleet,
        waiting,
        seat_key,
        mask: conf.mask || std::env::var_os("SWARMY_MASK").is_some(),
        color: std::env::var_os("NO_COLOR").is_none(),
        segments: conf.segments.clone(),
        width: columns
            .saturating_sub(conf.reserve as usize + theirs_w + 4)
            .max(20),
        lines: conf.lines,
    };
    let mine = render(&input, &x, now);
    match theirs {
        Some(t) => println!("{} │ {mine}", t.trim_end()),
        None => println!("{mine}"),
    }
    Ok(())
}

/// The status line the user had before: same stdin, its first line of output.
fn run_previous(cmd: &str, stdin: &str) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", cmd])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?
    } else {
        Command::new("sh")
            .args(["-c", cmd])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?
    };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const INPUT: &str = r#"{"session_id":"s1","model":{"display_name":"Opus"},"cost":{"total_cost_usd":4.2},
        "context_window":{"used_percentage":42.0},
        "rate_limits":{"five_hour":{"used_percentage":12.5,"resets_at":1795400000},
                       "seven_day":{"used_percentage":3,"resets_at":1795960000}}}"#;

    #[test]
    fn readings_come_from_rate_limits_and_win_when_newer() {
        let input = parse_input(INPUT);
        let at = Utc.timestamp_opt(1_795_380_000, 0).unwrap();
        let live = live_from(&input, Path::new("/x/.claude"), at).unwrap();
        assert_eq!(live.seven_day.unwrap().0, 3.0);
        assert_eq!(live.five_hour.unwrap().1.timestamp(), 1_795_400_000);

        // Older cached reading (95%) of the same account: the live one replaces it.
        let cached = Utilization {
            account_uuid: "acc".into(),
            fetched_at: at - chrono::Duration::hours(1),
            readings: vec![Reading {
                kind: WindowKind::SevenDay,
                utilization: 95.0,
                resets_at: at,
                fetched_at: at,
            }],
            extra_usage_enabled: false,
        };
        let m = merge(Some(cached.clone()), Some("acc"), Some(live.clone())).unwrap();
        let week = m
            .readings
            .iter()
            .find(|r| r.kind == WindowKind::SevenDay)
            .unwrap();
        assert_eq!((week.utilization, m.fetched_at), (3.0, at));

        // A newer cached reading stays; no account means nothing to attach the live one to.
        let newer = Utilization {
            fetched_at: at + chrono::Duration::minutes(1),
            ..cached.clone()
        };
        assert_eq!(
            merge(Some(newer.clone()), Some("acc"), Some(live.clone())),
            Some(newer)
        );
        assert_eq!(merge(None, None, Some(live)), None);
    }

    fn extra(fleet: Option<crate::fleet::Fleet>, width: usize) -> Extra {
        Extra {
            fleet,
            waiting: Vec::new(),
            seat_key: SeatId::from_stored("0123456789abcdef"),
            mask: false,
            color: false,
            segments: crate::config::StatuslineConf::default().segments,
            width,
            lines: 1,
        }
    }

    fn fleet(now: DateTime<Utc>) -> crate::fleet::Fleet {
        use crate::fleet::*;
        let mut f = Fleet {
            v: VERSION,
            written_at: now,
            ..Default::default()
        };
        f.needs_you = vec![
            NeedsYou {
                session_id: "other".into(),
                name: "shop-api".into(),
                what: "permission".into(),
                ..Default::default()
            },
            NeedsYou {
                session_id: "s1".into(),
                name: "me".into(),
                what: "question".into(),
                ..Default::default()
            },
            NeedsYou {
                session_id: "x".into(),
                name: "docs".into(),
                what: "question".into(),
                ..Default::default()
            },
        ];
        f.sessions.insert(
            "s1".into(),
            SessionCounts {
                agents: 3,
                working: 2,
                thinking: 1,
                burn_usd_h: 3.1,
                ..Default::default()
            },
        );
        f.seats.insert(
            SeatId::from_stored("0123456789abcdef"),
            SeatLimit {
                runs_out_at: Some(now + chrono::Duration::hours(7)),
                resets_at: Some(now + chrono::Duration::days(3)),
                ..Default::default()
            },
        );
        f
    }

    #[test]
    fn the_full_line_and_how_it_shortens() {
        let now = Utc.timestamp_opt(1_795_380_000, 0).unwrap();
        let input = parse_input(INPUT);
        let full = render(&input, &extra(Some(fleet(now)), 400), now);
        // The other two sessions need you (not this one), this session's agents, the week with
        // its runs-out time, 5 hours, context, cost, burn.
        assert!(full.starts_with("▲ 2 need you: shop-api, docs · swarmy n J │ ◐ 3 agents ●2 ◌1 │ week used 3% · runs out in 7h 00m ("), "{full}");
        assert!(
            full.ends_with(") │ 5h 12% │ ctx 42% │ $4.20 │ $3.1/h"),
            "{full}"
        );

        // Narrow: burn, cost, 5h go; then shorter forms; the alert and the week stay.
        let mid = render(&input, &extra(Some(fleet(now)), 80), now);
        assert!(mid.len() <= 80 * 3, "{mid}");
        assert!(mid.contains("▲") && mid.contains("week"), "{mid}");
        assert!(!mid.contains("$3.1/h") && !mid.contains("$4.20"), "{mid}");
        let tiny = render(&input, &extra(Some(fleet(now)), 30), now);
        assert!(tiny.starts_with("▲2 │ "), "{tiny}");
        assert!(tiny.contains("week 3%"), "{tiny}");
    }

    #[test]
    fn without_the_dashboard_or_limits() {
        let now = Utc::now();
        let input = parse_input(r#"{"session_id":"s","cost":{"total_cost_usd":0.5}}"#);
        assert!(live_from(&input, Path::new("/x"), now).is_none());
        assert_eq!(render(&input, &extra(None, 120), now), "$0.50");
        // The registry fallback still tells who waits; masked names are hidden.
        let mut x = extra(None, 120);
        x.waiting = vec![("other".into(), "client-x".into(), "permission".into(), None)];
        assert_eq!(
            render(&input, &x, now),
            "▲ client-x needs you: permission · swarmy n J │ $0.50"
        );
        x.mask = true;
        assert_eq!(render(&input, &x, now), "▲ 1 needs you · n J │ $0.50");
    }
}
