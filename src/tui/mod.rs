//! The live dashboard.
//!
//! A worker thread owns the index: it refreshes it every two seconds, builds the live snapshot,
//! and every ten seconds the heavier analytics. The UI thread only draws and handles keys, so
//! animations stay smooth while transcripts are being read.

mod app;
mod boot;
mod draw;
mod theme;
mod vault_ui;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;

use crate::claude::config_json;
use crate::claude::procs::ProcTable;
use crate::claude::registry::{self, LiveSession};
use crate::cli::context::Ctx;
use crate::cli::limits::{self, AccountReport};
use crate::core::period::{Period, Span};
use crate::index::query::{Dim, Filter, Row};
use crate::index::{Index, Progress, SeatInfo};
use crate::live::{self, Snapshot, ToolLine};
use crate::vault::{Login, Vault};

pub use app::App;
pub use boot::hash;

/// Heavier, slower-changing numbers for the analytics screens.
#[derive(Debug, Clone, Default)]
pub struct Analytics {
    pub period: Option<Period>,
    pub span: Option<Span>,
    pub group: Option<Dim>,
    pub groups: Vec<Row>,
    pub by_model: Vec<Row>,
    pub total: Option<Row>,
    pub days: Vec<Row>,
    pub heat: Vec<(String, String, f64)>,
    pub effort: Vec<(String, String, f64)>,
    pub substitutions: Vec<(String, String, u64)>,
    pub project_model: Vec<(String, String, f64)>,
    pub limits: Vec<AccountReport>,
    pub logins: Vec<Login>,
    pub seats: Vec<SeatRow>,
    pub transcripts_bytes: Vec<(String, u64)>,
    pub index_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct SeatRow {
    pub label: String,
    /// The confirmed account (None while a new login is unconfirmed).
    pub account: Option<String>,
    pub login: Option<String>,
    pub live: usize,
    pub state: crate::vault::SeatState,
}

#[derive(Debug, Clone, Default)]
pub struct SystemView {
    /// (seat, session name, pid, cpu %, bytes, top children (cmd, cpu, bytes))
    pub instances: Vec<(String, String, u32, f32, u64, Vec<(String, f32, u64)>)>,
    pub mem_total: u64,
    pub mem_available: u64,
    pub disk_free: Option<u64>,
}

pub enum Msg {
    Progress(Progress),
    Ready {
        seats: usize,
        accounts: usize,
        files: usize,
    },
    Snapshot(Box<Snapshot>, Vec<LiveSession>, Box<SystemView>),
    Analytics(Box<Analytics>),
    Timeline(String, Vec<ToolLine>),
    Error(String),
}

/// What the UI asks of the worker.
#[derive(Debug, Clone, Default)]
pub struct Wants {
    pub period: Option<Period>,
    pub group: Option<Dim>,
    pub agent: Option<live::Agent>,
    pub refresh_analytics: bool,
    /// The SYSTEM screen is open: keep the full process table fresh.
    pub system: bool,
}

pub fn run(ctx: Ctx, no_boot: bool, force_eco: bool) -> anyhow::Result<()> {
    let (tx, rx) = channel::<Msg>();
    let wants = Arc::new(Mutex::new(Wants {
        period: Some(Period::LimitWeek),
        group: Some(Dim::Project),
        ..Default::default()
    }));
    let w2 = wants.clone();
    let mask = ctx.mask;
    let theme = theme::Theme::named(ctx.config.theme.as_deref());
    let seats = ctx.seats.clone();
    let vault_dir = ctx.dirs.vault_dir();
    let home = ctx.dirs.home.clone();
    let eco = Arc::new(std::sync::atomic::AtomicBool::new(
        force_eco || crate::power::on_battery(),
    ));
    let eco2 = eco.clone();
    std::thread::spawn(move || worker(ctx, tx, w2, eco2, force_eco));

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let mut app = App::new(theme, mask, no_boot, seats, vault_dir, home);
    let res = ui_loop(&mut terminal, &mut app, &rx, &wants, &eco);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

fn ui_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &Receiver<Msg>,
    wants: &Arc<Mutex<Wants>>,
    eco: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<()> {
    loop {
        let started = Instant::now();
        app.eco = eco.load(std::sync::atomic::Ordering::Relaxed);
        // 10 frames a second for animations, 4 in eco mode, 2 when nothing moves.
        let frame_time = Duration::from_millis(if !app.animating() {
            500
        } else if app.eco {
            250
        } else {
            100
        });
        while let Ok(m) = rx.try_recv() {
            app.on_msg(m);
        }
        terminal.draw(|f| draw::draw(f, app))?;
        let timeout = frame_time
            .saturating_sub(started.elapsed())
            .max(Duration::from_millis(5));

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if app.on_key(k) {
                        return Ok(());
                    }
                    if let Some(vault_ui::External::AddLogin(label)) = app.vault.external.take() {
                        // Hand the terminal to Claude Code for /login, then take it back.
                        let _ = execute!(std::io::stdout(), DisableMouseCapture);
                        ratatui::restore();
                        app.run_add_login(&label);
                        *terminal = ratatui::init();
                        let _ = execute!(std::io::stdout(), EnableMouseCapture);
                        terminal.clear()?;
                    }
                }
                Event::Mouse(m) => app.on_mouse(m),
                Event::Resize(..) => {}
                _ => {}
            }
            let mut w = wants.lock().unwrap();
            w.period = Some(app.period.clone());
            w.group = Some(app.group);
            w.agent = app.selected_agent().cloned();
            w.system = app.screen == app::Screen::System;
            if app.take_analytics_request() {
                w.refresh_analytics = true;
            }
        }
    }
}

/// Records the dashboard headlessly: `frames` frames at 10 per second into `out` (JSON lines,
/// one frame per line, rows of `[text, fg, bg, bold]` runs), pressing `keys` at given frames.
pub fn record(
    ctx: Ctx,
    out: &std::path::Path,
    width: u16,
    height: u16,
    frames: usize,
    keys: &[(usize, char)],
) -> anyhow::Result<()> {
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::Color;
    let (tx, rx) = channel::<Msg>();
    let wants = Arc::new(Mutex::new(Wants {
        period: Some(Period::LimitWeek),
        group: Some(Dim::Project),
        ..Default::default()
    }));
    let w2 = wants.clone();
    let theme = theme::Theme::named(ctx.config.theme.as_deref());
    let (seats, vault_dir, home) = (
        ctx.seats.clone(),
        ctx.dirs.vault_dir(),
        ctx.dirs.home.clone(),
    );
    let eco = Arc::new(std::sync::atomic::AtomicBool::new(false));
    std::thread::spawn(move || worker(ctx, tx, w2, eco, false));
    let mut term = ratatui::Terminal::new(TestBackend::new(width, height))?;
    let mut app = App::new(theme, false, false, seats, vault_dir, home);
    let col = |c: Color| match c {
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        _ => None,
    };
    let mut file = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut written = 0;
    let mut frame = 0;
    // Wait for the data before the clock of the recording starts (boot runs meanwhile).
    while written < frames {
        let t = Instant::now();
        while let Ok(m) = rx.try_recv() {
            app.on_msg(m);
        }
        let live = app.boot.is_none();
        if live {
            for (_, k) in keys.iter().filter(|(f, _)| *f == frame) {
                let code = match k {
                    '\n' => KeyCode::Enter,
                    '\x1b' => KeyCode::Esc,
                    c => KeyCode::Char(*c),
                };
                app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
                let mut w = wants.lock().unwrap();
                w.period = Some(app.period.clone());
                w.group = Some(app.group);
                w.agent = app.selected_agent().cloned();
                w.system = app.screen == app::Screen::System;
                if app.take_analytics_request() {
                    w.refresh_analytics = true;
                }
            }
        }
        term.draw(|f| draw::draw(f, &mut app))?;
        if live && !app.analytics.limits.is_empty() || app.boot.is_some() && frame == usize::MAX {
            let buf = term.backend().buffer();
            let mut rows = Vec::new();
            for y in 0..buf.area.height {
                let mut runs: Vec<(String, Option<String>, Option<String>, bool)> = Vec::new();
                for x in 0..buf.area.width {
                    let c = &buf[(x, y)];
                    let key = (
                        col(c.fg),
                        col(c.bg),
                        c.modifier.contains(ratatui::style::Modifier::BOLD),
                    );
                    match runs.last_mut() {
                        Some(r) if (r.1.clone(), r.2.clone(), r.3) == key => {
                            r.0.push_str(c.symbol())
                        }
                        _ => runs.push((c.symbol().to_string(), key.0, key.1, key.2)),
                    }
                }
                rows.push(runs);
            }
            use std::io::Write as _;
            writeln!(file, "{}", serde_json::to_string(&rows)?)?;
            written += 1;
            frame += 1;
        }
        std::thread::sleep(Duration::from_millis(100).saturating_sub(t.elapsed()));
    }
    Ok(())
}

/// Modification times of every seat's credentials and `.claude.json`: two `stat`s per seat.
fn login_files_stamp(ctx: &Ctx) -> Vec<Option<std::time::SystemTime>> {
    ctx.seats
        .iter()
        .flat_map(|s| [s.credentials_file(), s.claude_json.clone()])
        .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .collect()
}

fn reload_seat_info(ctx: &mut Ctx) {
    ctx.seat_info = ctx
        .seats
        .iter()
        .map(|s| {
            let cj = config_json::read(&s.claude_json).unwrap_or_default();
            SeatInfo {
                label: s.label.clone(),
                account: cj.account,
                utilization: cj.utilization,
            }
        })
        .collect();
    ctx.drop_unconfirmed();
}

fn worker(
    mut ctx: Ctx,
    tx: Sender<Msg>,
    wants: Arc<Mutex<Wants>>,
    eco: Arc<std::sync::atomic::AtomicBool>,
    force_eco: bool,
) {
    let send = |m: Msg| tx.send(m).is_ok();
    let mut idx = match Index::open(&ctx.dirs.index_file()) {
        Ok(i) => i,
        Err(e) => {
            send(Msg::Error(format!("{e:#}")));
            return;
        }
    };
    let mut resolver = crate::claude::project::Resolver::new(ctx.config.project.clone());
    let mut progress = |p: Progress| {
        let _ = tx.send(Msg::Progress(p));
    };
    match idx.refresh(
        &ctx.seats,
        &ctx.seat_info,
        &mut resolver,
        &ctx.prices,
        &mut progress,
    ) {
        Ok(s) => {
            let accounts = ctx
                .seat_info
                .iter()
                .filter_map(|s| s.account.as_ref().map(|a| a.account_uuid.clone()))
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            if !send(Msg::Ready {
                seats: ctx.seats.len(),
                accounts,
                files: s.files_total,
            }) {
                return;
            }
        }
        Err(e) => {
            send(Msg::Error(format!("{e:#}")));
            return;
        }
    }
    let mut sys = sysinfo::System::new().ok();
    let opts = live::Options {
        stall_minutes: ctx.config.stall_minutes() as i64,
        ..Default::default()
    };
    let mut last_analytics: Option<Instant> = None;
    let mut last_info = Instant::now();
    let mut last_disk: Option<Instant> = None;
    let mut disk_cache: Vec<(String, u64)> = Vec::new();
    let mut tick = 0u64;
    let mut last_full = Instant::now();
    let mut login_stamp = login_files_stamp(&ctx);
    let mut last_power = Instant::now();
    let mut last_procs = Instant::now() - Duration::from_secs(60);
    let mut slow: Option<(Instant, Vec<Row>, Vec<(String, String, f64)>)> = None;
    loop {
        tick += 1;
        ctx.now = Utc::now();
        let tp = Instant::now();
        let want_system = wants.lock().unwrap().system;
        if let Some(s) = sys.as_mut() {
            if want_system && last_procs.elapsed() > Duration::from_secs(3) {
                ctx.procs = ProcTable::snapshot_with_cpu(s);
                last_procs = Instant::now();
            } else if !want_system || last_procs.elapsed() > Duration::from_secs(3) {
                let pids: Vec<u32> = ctx
                    .seats
                    .iter()
                    .flat_map(|st| registry::session_pids(&st.sessions_dir()))
                    .collect();
                ctx.procs = ProcTable::for_pids(s, &pids);
            }
        }
        if std::env::var_os("AGENTMANA_DEBUG").is_some() {
            eprintln!("procs {:?} ({} nodes)", tp.elapsed(), ctx.procs.nodes.len());
        }
        // A login file changed (a refresh, a /login, a copied backup): re-read at once.
        let stamp = login_files_stamp(&ctx);
        if stamp != login_stamp || last_info.elapsed() > Duration::from_secs(20) {
            if stamp != login_stamp {
                wants.lock().unwrap().refresh_analytics = true;
                login_stamp = stamp;
            }
            reload_seat_info(&mut ctx);
            last_info = Instant::now();
        }
        let dbg = std::env::var_os("AGENTMANA_DEBUG").is_some();
        let t0 = Instant::now();
        let live_sessions: Vec<LiveSession> = ctx
            .seats
            .iter()
            .flat_map(|s| registry::live_sessions(&s.label, &s.sessions_dir(), &ctx.procs))
            .collect();
        let res = if last_full.elapsed() > Duration::from_secs(60) {
            last_full = Instant::now();
            let mut quiet = |_p: Progress| {};
            idx.refresh(
                &ctx.seats,
                &ctx.seat_info,
                &mut resolver,
                &ctx.prices,
                &mut quiet,
            )
        } else {
            let keys: Vec<(String, String)> = live_sessions
                .iter()
                .map(|l| (l.seat.clone(), l.session_id.clone()))
                .collect();
            idx.refresh_hot(
                &ctx.seats,
                &keys,
                &ctx.seat_info,
                &mut resolver,
                &ctx.prices,
            )
        };
        if let Err(e) = res {
            send(Msg::Error(format!("{e:#}")));
        }
        let t1 = Instant::now();
        let built = live::build(&idx, &live_sessions, &opts, ctx.now);
        if dbg {
            eprintln!("refresh {:?} build {:?}", t1 - t0, t1.elapsed());
        }
        match built {
            Ok(snap) => {
                let sysview = system_view(&ctx, &live_sessions, sys.as_ref());
                if !send(Msg::Snapshot(
                    Box::new(snap),
                    live_sessions.clone(),
                    Box::new(sysview),
                )) {
                    return;
                }
            }
            Err(e) => {
                send(Msg::Error(format!("{e:#}")));
            }
        }
        let w = wants.lock().unwrap().clone();
        if let Some(a) = &w.agent
            && let Ok(t) = live::tool_timeline(&idx, a, 40)
            && !send(Msg::Timeline(a.key(), t))
        {
            return;
        }
        let due = last_analytics.is_none_or(|t| t.elapsed() > Duration::from_secs(15))
            || w.refresh_analytics;
        if due {
            if last_disk.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) {
                disk_cache = ctx
                    .seats
                    .iter()
                    .map(|s| (s.label.clone(), dir_size(&s.projects_dir())))
                    .collect();
                last_disk = Some(Instant::now());
            }
            if wants.lock().unwrap().refresh_analytics {
                // A login may have just changed: re-read the seats' accounts.
                reload_seat_info(&mut ctx);
            }
            wants.lock().unwrap().refresh_analytics = false;
            let ta = Instant::now();
            // Daily bars and the heatmap move slowly: every five minutes is enough.
            if slow
                .as_ref()
                .is_none_or(|(t, ..)| t.elapsed() > Duration::from_secs(300))
            {
                let f = Filter::default();
                let days_span = Span {
                    start: ctx.now - chrono::Duration::days(14),
                    end: ctx.now + chrono::Duration::seconds(1),
                };
                let heat_span = Span {
                    start: ctx.now - chrono::Duration::days(28),
                    end: ctx.now + chrono::Duration::seconds(1),
                };
                if let (Ok(d), Ok(h)) = (
                    idx.spend_by(Dim::Day, days_span, &f),
                    idx.matrix(Dim::Weekday, Dim::Hour, heat_span, &f),
                ) {
                    slow = Some((Instant::now(), d, h));
                }
            }
            let res = analytics(
                &ctx,
                &idx,
                &w,
                &disk_cache,
                slow.as_ref().map(|(_, d, h)| (d.clone(), h.clone())),
            );
            if dbg {
                eprintln!("analytics {:?}", ta.elapsed());
            }
            match res {
                Ok(a) => {
                    if !send(Msg::Analytics(Box::new(a))) {
                        return;
                    }
                }
                Err(e) => {
                    send(Msg::Error(format!("{e:#}")));
                }
            }
            last_analytics = Some(Instant::now());
        }
        let _ = tick;
        if last_power.elapsed() > Duration::from_secs(30) {
            eco.store(
                force_eco || crate::power::on_battery(),
                std::sync::atomic::Ordering::Relaxed,
            );
            last_power = Instant::now();
        }
        // One second per tick (three on battery), but answer UI requests sooner.
        let steps = if eco.load(std::sync::atomic::Ordering::Relaxed) {
            30
        } else {
            10
        };
        for _ in 0..steps {
            std::thread::sleep(Duration::from_millis(100));
            if wants.lock().unwrap().refresh_analytics {
                break;
            }
        }
    }
}

fn analytics(
    ctx: &Ctx,
    idx: &Index,
    w: &Wants,
    disk: &[(String, u64)],
    slow: Option<(Vec<Row>, Vec<(String, String, f64)>)>,
) -> anyhow::Result<Analytics> {
    let (days_cached, heat_cached) = slow.unwrap_or_default();
    let period = w.period.clone().unwrap_or(Period::LimitWeek);
    let span = period.span(ctx.now, ctx.limit_week(None));
    let f = Filter::default();
    let group = w.group.unwrap_or(Dim::Project);
    let mut groups = idx.spend_by(group, span, &f)?;
    if group == Dim::Account {
        for r in &mut groups {
            r.label = ctx.account_name(&r.key);
        }
    }
    let mut vault_logins = Vec::new();
    let mut seats = Vec::new();
    let mut states = std::collections::BTreeMap::new();
    if let Ok(mut v) = Vault::open(&ctx.dirs.vault_dir()) {
        states = v.seat_states(&ctx.seats).unwrap_or_default();
        vault_logins = v.logins().to_vec();
    }
    for (s, si) in ctx.seats.iter().zip(&ctx.seat_info) {
        let state = states.get(&s.label).cloned().unwrap_or_default();
        seats.push(SeatRow {
            label: s.label.clone(),
            account: if state.unverified_since.is_some() {
                None
            } else {
                si.account.as_ref().map(|a| a.account_uuid.clone())
            },
            login: state.saved_as.clone().or_else(|| {
                vault_logins
                    .iter()
                    .find(|l| l.mounted.as_deref() == Some(&s.label))
                    .map(|l| l.label.clone())
            }),
            live: registry::live_sessions(&s.label, &s.sessions_dir(), &ctx.procs).len(),
            state,
        });
    }
    let dbg = std::env::var_os("AGENTMANA_DEBUG").is_some();
    macro_rules! timed {
        ($name:expr, $e:expr) => {{
            let t = Instant::now();
            let v = $e;
            if dbg {
                eprintln!("  {} {:?}", $name, t.elapsed());
            }
            v
        }};
    }
    Ok(Analytics {
        period: Some(period),
        span: Some(span),
        group: Some(group),
        groups,
        by_model: timed!("by_model", idx.spend_by(Dim::Model, span, &f)?),
        total: timed!("total", Some(idx.total(span, &f)?)),
        days: days_cached,
        heat: heat_cached,
        effort: timed!("effort", idx.matrix(Dim::Model, Dim::Effort, span, &f)?),
        substitutions: timed!("subs", idx.model_substitutions(span, &f)?),
        project_model: timed!(
            "proj×model",
            idx.matrix(Dim::Project, Dim::Model, span, &f)?
        ),
        limits: timed!("limits", limits::all_reports(ctx, idx)?),
        logins: vault_logins,
        seats,
        transcripts_bytes: disk.to_vec(),
        index_bytes: std::fs::metadata(ctx.dirs.index_file())
            .map(|m| m.len())
            .unwrap_or(0),
    })
}

fn system_view(ctx: &Ctx, live: &[LiveSession], sys: Option<&sysinfo::System>) -> SystemView {
    let mut v = SystemView::default();
    for l in live {
        let (cpu, mem) = ctx.procs.tree_usage(l.pid);
        let mut kids: Vec<(String, f32, u64)> = ctx
            .procs
            .descendants(l.pid)
            .into_iter()
            .map(|n| {
                let cmd = if n.cmd.is_empty() {
                    n.name.clone()
                } else {
                    n.cmd.clone()
                };
                (cmd, n.cpu, n.memory)
            })
            .collect();
        kids.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
        kids.truncate(6);
        v.instances.push((
            l.seat.clone(),
            l.name
                .clone()
                .unwrap_or_else(|| l.session_id[..8].to_string()),
            l.pid,
            cpu,
            mem,
            kids,
        ));
    }
    if let Some(s) = sys {
        v.mem_total = s.total_memory();
        v.mem_available = s.available_memory();
    }
    v.disk_free = free_space(&ctx.dirs.home);
    v
}

fn free_space(path: &std::path::Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

fn dir_size(p: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}
