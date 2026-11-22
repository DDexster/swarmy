//! The worker thread: owns the index, refreshes it every tick, builds the live snapshot and, on
//! the cadences of `jobs::JOBS`, the heavier analytics. It sends everything to the UI thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::Utc;

use super::analytics::{self, SlowParts};
use super::jobs::{Job, Schedule, TICK_STEP};
use super::{Msg, SystemView, Wants, debug};
use crate::app::context::Ctx;
use crate::claude::procs::ProcTable;
use crate::claude::registry::{self, LiveSession};
use crate::core::period::Span;
use crate::index::query::{Dim, Filter};
use crate::index::{Index, Progress};
use crate::limits;
use crate::live;

pub(super) fn worker(
    mut ctx: Ctx,
    tx: Sender<Msg>,
    wants: Arc<Mutex<Wants>>,
    eco: Arc<AtomicBool>,
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
    let mut jobs = Schedule::new(eco.load(Ordering::Relaxed));
    if debug() {
        super::jobs::print_table();
    }
    let mut disk_cache: Vec<(String, u64)> = Vec::new();
    let mut login_stamp = login_files_stamp(&ctx);
    let mut seats_changed = false;
    let mut sl_stamp = statusline_stamp(&ctx);
    let mut past_sent = false;
    let mut prev_snap: Option<live::Snapshot> = None;
    let mut prev_analytics: Option<super::Analytics> = None;
    let mut fleet_last: Option<crate::fleet::Fleet> = None;
    let mut slow: Option<SlowParts> = None;
    loop {
        let tick_start = Instant::now();
        let mut marks: Vec<(&str, Instant)> = Vec::new();
        ctx.now = Utc::now();
        let tp = Instant::now();
        let want_system = wants.lock().unwrap().system;
        if let Some(s) = sys.as_mut() {
            if want_system && jobs.due(Job::SystemProcs) {
                ctx.procs = ProcTable::snapshot_with_cpu(s);
                jobs.done(Job::SystemProcs);
            } else if !want_system || jobs.due(Job::SystemProcs) {
                let pids: Vec<u32> = ctx
                    .seats
                    .iter()
                    .flat_map(|st| registry::session_pids(&st.sessions_dir()))
                    .collect();
                ctx.procs = ProcTable::for_pids(s, &pids);
            }
        }
        if debug() {
            eprintln!("procs {:?} ({} nodes)", tp.elapsed(), ctx.procs.nodes.len());
        }
        marks.push(("procs", Instant::now()));
        // A fresh status-line reading: limits are exact again, show them.
        let sl = statusline_stamp(&ctx);
        if sl != sl_stamp && jobs.due(Job::StatusLine) {
            sl_stamp = sl;
            jobs.done(Job::StatusLine);
            if let Err(e) = ctx.reload_seat_info() {
                send(Msg::Error(format!("{e:#}")));
            }
            jobs.done(Job::SeatInfo);
            seats_changed = true;
        }
        // A login file changed (a refresh, a /login, a copied backup): re-read at once.
        let stamp = login_files_stamp(&ctx);
        if stamp != login_stamp || jobs.due(Job::SeatInfo) {
            if stamp != login_stamp {
                // A login changed: refresh seats and limits now, not the period aggregates.
                seats_changed = true;
                login_stamp = stamp;
            }
            if let Err(e) = ctx.reload_seat_info() {
                send(Msg::Error(format!("{e:#}")));
            }
            jobs.done(Job::SeatInfo);
        }
        marks.push(("seatinfo", Instant::now()));
        let dbg = debug();
        let t0 = Instant::now();
        let live_sessions: Vec<LiveSession> = ctx
            .seats
            .iter()
            .flat_map(|s| registry::live_sessions(&s.label, &s.sessions_dir(), &ctx.procs))
            .collect();
        let res = if jobs.due(Job::FullRefresh) {
            jobs.done(Job::FullRefresh);
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
        marks.push(("sessions+refresh", Instant::now()));
        let t1 = Instant::now();
        let heavy = jobs.due(Job::HeavyParts);
        let built = live::build(
            &idx,
            &live_sessions,
            &ctx.procs,
            &opts,
            ctx.now,
            if heavy { None } else { prev_snap.as_ref() },
            prev_snap.as_ref(),
        );
        if heavy {
            jobs.done(Job::HeavyParts);
        }
        if let Ok(s) = &built {
            prev_snap = Some(s.clone());
        }
        if dbg {
            eprintln!("refresh {:?} build {:?}", t1 - t0, t1.elapsed());
        }
        match built {
            Ok(snap) => {
                // The status line's summary: when something changed (at most every 2 s), and
                // at least every 5 s so it stays fresh.
                if jobs.due(Job::Fleet) {
                    let fl = fleet_of(
                        &ctx,
                        &snap,
                        prev_analytics
                            .as_ref()
                            .map(|a| a.limits.as_slice())
                            .unwrap_or(&[]),
                    );
                    let changed = Some(&fl) != fleet_last.as_ref();
                    if changed || jobs.due(Job::FleetKeepAlive) {
                        let mut f = fl.clone();
                        f.written_at = Utc::now();
                        let _ = crate::fleet::write(&crate::fleet::path(&ctx.dirs.data), &f);
                        fleet_last = Some(fl);
                        jobs.done(Job::Fleet);
                        jobs.done(Job::FleetKeepAlive);
                    }
                }
                // Only the SYSTEM panel shows this; listing disks is not free.
                let sysview = if want_system {
                    system_view(&ctx, &live_sessions, sys.as_ref())
                } else {
                    SystemView::default()
                };
                if !past_sent {
                    past_sent = true;
                    if let Ok(p) =
                        live::past_events(&idx, ctx.now - chrono::Duration::hours(3), 200)
                    {
                        let _ = send(Msg::Past(p));
                    }
                }
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
        marks.push(("build+send", Instant::now()));
        let w = wants.lock().unwrap().clone();
        let tt = Instant::now();
        if let Some(a) = &w.agent
            && let Ok(t) = live::tool_timeline(&idx, a, 200)
            && !send(Msg::Timeline(
                a.key(),
                t,
                live::last_message(&idx, a),
                live::life(&idx, a).unwrap_or_default(),
            ))
        {
            return;
        }
        marks.push(("timeline", Instant::now()));
        if dbg {
            eprintln!("timeline {:?}", tt.elapsed());
        }
        let seats_now = std::mem::take(&mut seats_changed);
        if jobs.due(Job::Analytics) || w.refresh_analytics || seats_now {
            if jobs.due(Job::DiskSizes) {
                disk_cache = ctx
                    .seats
                    .iter()
                    .map(|s| (s.label.clone(), dir_size(&s.projects_dir())))
                    .collect();
                jobs.done(Job::DiskSizes);
            }
            if wants.lock().unwrap().refresh_analytics {
                // A login may have just changed: re-read the seats' accounts.
                if let Err(e) = ctx.reload_seat_info() {
                    send(Msg::Error(format!("{e:#}")));
                }
            }
            wants.lock().unwrap().refresh_analytics = false;
            let ta = Instant::now();
            if jobs.due(Job::DaysHeat)
                && let Some(parts) = days_and_heat(&ctx, &idx)
            {
                slow = Some(parts);
                jobs.done(Job::DaysHeat);
            }
            let full = w.refresh_analytics || jobs.due(Job::AnalyticsFull);
            let limits_due = full || seats_now || jobs.due(Job::Limits);
            let res = analytics::analytics(analytics::Inputs {
                ctx: &ctx,
                idx: &idx,
                wants: &w,
                jobs: &jobs,
                disk: &disk_cache,
                slow: slow.clone(),
                reuse_period: if full { None } else { prev_analytics.as_ref() },
                reuse_limits: if limits_due {
                    None
                } else {
                    prev_analytics.as_ref().map(|p| p.limits.as_slice())
                },
                last: prev_analytics.as_ref(),
            });
            if limits_due && res.is_ok() {
                jobs.done(Job::Limits);
            }
            if full && res.is_ok() {
                jobs.done(Job::AnalyticsFull);
            }
            if dbg {
                eprintln!("analytics {:?}", ta.elapsed());
            }
            match res {
                Ok(a) => {
                    prev_analytics = Some(a.clone());
                    if !send(Msg::Analytics(Box::new(a))) {
                        return;
                    }
                }
                Err(e) => {
                    send(Msg::Error(format!("{e:#}")));
                }
            }
            jobs.done(Job::Analytics);
        }
        if jobs.due(Job::Power) {
            eco.store(force_eco || crate::power::on_battery(), Ordering::Relaxed);
            jobs.done(Job::Power);
        }
        if debug() {
            let mut prev = tick_start;
            let parts: Vec<String> = marks
                .iter()
                .map(|(n, t)| {
                    let d = *t - prev;
                    prev = *t;
                    format!("{n}={:.1}", d.as_secs_f64() * 1000.0)
                })
                .collect();
            eprintln!(
                "tick {:.1}ms {}",
                tick_start.elapsed().as_secs_f64() * 1000.0,
                parts.join(" ")
            );
        }
        // One tick (longer on battery), but answer UI requests sooner.
        jobs.eco = eco.load(Ordering::Relaxed);
        for _ in 0..jobs.tick_steps() {
            std::thread::sleep(TICK_STEP);
            if wants.lock().unwrap().refresh_analytics {
                break;
            }
        }
    }
}

/// Day bars (14 days), the weekday × hour heat map (28 days) and day × model spend.
fn days_and_heat(ctx: &Ctx, idx: &Index) -> Option<SlowParts> {
    let f = Filter::default();
    let days_span = Span {
        start: ctx.now - chrono::Duration::days(14),
        end: ctx.now + chrono::Duration::seconds(1),
    };
    let heat_span = Span {
        start: ctx.now - chrono::Duration::days(28),
        end: ctx.now + chrono::Duration::seconds(1),
    };
    let (Ok(d), Ok(h), Ok(dm)) = (
        idx.spend_by(Dim::Day, days_span, &f),
        idx.matrix(Dim::Weekday, Dim::Hour, heat_span, &f),
        idx.days_by_model(days_span),
    ) else {
        return None;
    };
    Some((d, h, dm))
}

/// Modification times of every seat's credentials file (`.claude.json` changes every few seconds
/// and is re-read on a timer instead).
fn login_files_stamp(ctx: &Ctx) -> Vec<Option<std::time::SystemTime>> {
    ctx.seats
        .iter()
        .map(|s| s.credentials_file())
        .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .collect()
}

/// The fleet summary for the status line (`fleet.rs`); `written_at` is set by the caller.
fn fleet_of(
    ctx: &Ctx,
    snap: &live::Snapshot,
    limits: &[limits::AccountReport],
) -> crate::fleet::Fleet {
    use crate::fleet::{Fleet, NeedsYou, SeatLimit};
    let mut f = Fleet {
        v: crate::fleet::VERSION,
        writer_pid: std::process::id(),
        ..Default::default()
    };
    for a in &snap.agents {
        let main = snap
            .agents
            .iter()
            .find(|m| m.is_main() && m.seat == a.seat && m.session_id == a.session_id);
        if a.is_main() {
            if a.state != live::State::Gone {
                f.counts.sessions += 1;
            }
            f.sessions
                .entry(a.session_id.clone())
                .or_default()
                .burn_usd_h = a.session_burn;
        } else if a.state.is_active()
            || matches!(
                a.state,
                live::State::NeedsYou(_) | live::State::Stalled | live::State::WaitingAgents(_)
            )
        {
            f.counts.agents += 1;
            f.sessions.entry(a.session_id.clone()).or_default().agents += 1;
        }
        let sc = f.sessions.entry(a.session_id.clone()).or_default();
        let (w, t, wt, st) = match &a.state {
            live::State::Tool { .. } => (1, 0, 0, 0),
            live::State::Thinking => (0, 1, 0, 0),
            live::State::WaitingAgents(_) => (0, 0, 1, 0),
            live::State::Stalled => (0, 0, 0, 1),
            _ => (0, 0, 0, 0),
        };
        if !a.is_main() {
            sc.working += w;
            sc.thinking += t;
            sc.waiting += wt;
            sc.stalled += st;
        }
        f.counts.working += w;
        f.counts.thinking += t;
        f.counts.waiting += wt;
        f.counts.stalled += st;
        if let live::State::NeedsYou(what) = &a.state {
            let name = main
                .map(|m| m.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| a.project.clone());
            f.needs_you.push(NeedsYou {
                session_id: a.session_id.clone(),
                seat: a.seat.clone(),
                name,
                what: what.clone(),
                since: Some(a.since),
                tmux: main.and_then(|m| m.tmux.clone()).or_else(|| a.tmux.clone()),
            });
        }
    }
    f.needs_you.sort_by_key(|n| n.since);
    for (s, si) in ctx.seats.iter().zip(&ctx.seat_info) {
        let key = crate::claude::statusline::file_for(&ctx.dirs.data, &s.dir)
            .file_stem()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(acct) = si.account.as_ref().map(|a| a.account_uuid.clone()) else {
            continue;
        };
        let week = limits
            .iter()
            .find(|r| r.account_uuid == acct)
            .and_then(|r| {
                r.windows.iter().find(|w| {
                    w.reading.kind == crate::core::limits::WindowKind::SevenDay && !w.reset_passed
                })
            });
        f.seats.insert(
            key,
            SeatLimit {
                label: s.label.clone(),
                account: ctx.account_name(&acct),
                week_used: week.map(|w| w.estimate_now),
                runs_out_at: week.and_then(|w| w.eta).map(|d| ctx.now + d),
                resets_at: week.map(|w| w.reading.resets_at),
            },
        );
    }
    f
}

/// Modification times of the seats' status-line readings (one `stat` per seat).
fn statusline_stamp(ctx: &Ctx) -> Vec<Option<std::time::SystemTime>> {
    ctx.seats
        .iter()
        .map(|s| crate::claude::statusline::file_for(&ctx.dirs.data, &s.dir))
        .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .collect()
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
