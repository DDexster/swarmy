//! The live state of every agent: who is thinking, running a tool, waiting for the human,
//! waiting for its children, stalled or done. Built from the index (freshly refreshed), the
//! session registry and the process table.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::claude::registry::LiveSession;
use crate::core::models::Family;
use crate::core::tools::ToolClass;
use crate::index::live::{AgentKey, AgentRow, DoneAgent};
use crate::index::{Index, from_ms, ms};

mod state;
pub use state::{Options, State, StateInputs, agent_state};

#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    pub seat: String,
    pub session_id: String,
    /// Empty for the main thread.
    pub agent_id: String,
    pub parent: Option<String>,
    pub depth: u32,
    pub name: String,
    pub agent_type: Option<String>,
    pub project: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub state: State,
    /// When the current state began (tool start, last activity…).
    pub since: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub cost: f64,
    pub requests: u64,
    /// Prompt size of the last request: how full the context is.
    pub context_tokens: u64,
    /// USD per minute over the last 15 minutes.
    pub spark: Vec<f64>,
    /// What the agent did in each of the last 30 minutes (oldest first), see `Lane`.
    pub lane: Vec<Lane>,
    pub tmux: Option<String>,
    pub pid: Option<u32>,
    pub account: Option<String>,
    /// When the agent first appeared: the stable sort key of the tree.
    pub first_seen: DateTime<Utc>,
    /// Main threads only: the whole session (main thread and all its agents).
    pub session_cost: f64,
    /// Main threads only: session spend over the last hour.
    pub session_burn: f64,
    /// Main threads only: CPU % and resident bytes of the Claude Code process.
    pub cpu: f32,
    pub memory: u64,
    /// Time of the last request, to tell whether anything changed since the last snapshot.
    pub last_request_ts: Option<i64>,
}

impl Agent {
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.seat, self.session_id, self.agent_id)
    }

    pub fn family(&self) -> Family {
        self.model
            .as_deref()
            .map(Family::of)
            .unwrap_or(Family::Other)
    }

    pub fn is_main(&self) -> bool {
        self.agent_id.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolLine {
    pub ts: DateTime<Utc>,
    pub name: String,
    pub summary: String,
    pub done: Option<DateTime<Utc>>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub taken_at: Option<DateTime<Utc>>,
    /// Ordered as a forest: each main thread followed by its descendants.
    pub agents: Vec<Agent>,
    /// USD per minute for the last 60 minutes, per model family.
    pub burn: BTreeMap<String, Vec<f64>>,
    pub burn_per_hour: f64,
    pub today_cost: f64,
    pub errors_last_hour: Vec<(String, u64)>,
    /// Agents that finished in the last hours, newest first.
    pub recent_done: Vec<DoneRow>,
    /// Files edited by more than one agent in the last 30 minutes.
    pub conflicts: Vec<(String, u32)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoneRow {
    pub name: String,
    pub project: String,
    pub model: Option<String>,
    pub started: DateTime<Utc>,
    pub finished: DateTime<Utc>,
    pub cost: f64,
}

impl Snapshot {
    pub fn count(&self, f: impl Fn(&State) -> bool) -> usize {
        self.agents.iter().filter(|a| f(&a.state)).count()
    }
}

pub fn build(
    idx: &Index,
    live: &[LiveSession],
    procs: &crate::claude::procs::ProcTable,
    opts: &Options,
    now: DateTime<Utc>,
    // Session totals and the recently-done list change slowly: when `prev` is given they are
    // carried over instead of recomputed.
    prev: Option<&Snapshot>,
    // The previous snapshot, always: per-agent totals are reused when nothing new arrived.
    prev_all: Option<&Snapshot>,
) -> anyhow::Result<Snapshot> {
    let recent = now - Duration::minutes(opts.stall_minutes.max(30));
    let live_keys: HashSet<(String, String)> = live
        .iter()
        .map(|l| (l.seat.clone(), l.session_id.clone()))
        .collect();

    let mut rows: Vec<AgentRow> = Vec::new();
    for row in idx.live_agent_rows(recent, now - Duration::hours(24), &live_keys)? {
        let is_live_main = row.agent_id.is_empty()
            && live_keys.contains(&(row.seat.clone(), row.session_id.clone()));
        let recent_enough = row.last_ts.is_some_and(|t| t >= ms(recent));
        if row.agent_id.is_empty() && !is_live_main && !recent_enough {
            continue;
        }
        rows.push(row);
    }
    // Live sessions that have no transcript yet still show up.
    for l in live {
        if !rows
            .iter()
            .any(|r| r.agent_id.is_empty() && r.seat == l.seat && r.session_id == l.session_id)
        {
            rows.push(AgentRow {
                seat: l.seat.clone(),
                session_id: l.session_id.clone(),
                agent_id: String::new(),
                kind: "main".into(),
                agent_type: None,
                description: None,
                parent_agent_id: None,
                depth: Some(0),
                requested_model: None,
                effort: None,
                finished: true,
                last_ts: None,
                first_ts: Some(l.started_at.timestamp_millis()),
                title: l.name.clone(),
                cwd: Some(l.cwd.clone()),
            });
        }
    }
    let projects = idx.project_names()?;
    let spark_start = now - Duration::minutes(15);

    let mut agents = Vec::new();
    for r in &rows {
        let key = AgentKey {
            seat: &r.seat,
            session_id: &r.session_id,
            agent_id: &r.agent_id,
        };
        let last_req = idx.last_request(key)?.unwrap_or_default();
        let req_ts = last_req.ts;
        // Nothing new since the previous snapshot: reuse its totals and sparkline.
        let unchanged = prev_all
            .and_then(|p| {
                p.agents.iter().find(|o| {
                    o.seat == r.seat && o.session_id == r.session_id && o.agent_id == r.agent_id
                })
            })
            .filter(|o| o.last_request_ts == req_ts);
        let (requests, cost) = match unchanged {
            Some(o) => (o.requests, o.cost),
            None => idx.agent_totals(key)?,
        };
        let tool = match req_ts {
            Some(t) => idx.open_tool(key, t)?,
            None => None,
        };
        let done_ts = idx.last_tool_done(key)?;
        let reuse_spark = unchanged
            .filter(|_| prev.is_some())
            .map(|o| o.spark.clone());
        let spark = match reuse_spark {
            Some(sp) => sp,
            None => {
                let mut sp = vec![0.0; 15];
                for (i, c) in idx.agent_spend_per_minute(key, spark_start)? {
                    if (0..15).contains(&i) {
                        sp[i as usize] = c;
                    }
                }
                sp
            }
        };
        let last = [r.last_ts, req_ts, done_ts]
            .into_iter()
            .flatten()
            .max()
            .map(from_ms)
            .unwrap_or(now);
        let live_session = live
            .iter()
            .find(|l| l.seat == r.seat && l.session_id == r.session_id);
        let Some((state, since)) = agent_state(
            &StateInputs {
                is_main: r.agent_id.is_empty(),
                finished: r.finished,
                session: live_session,
                tool: tool.as_ref(),
                last,
            },
            opts,
            now,
        ) else {
            continue;
        };
        let name = if r.agent_id.is_empty() {
            live_session
                .and_then(|l| l.name.clone())
                .or_else(|| r.title.clone())
                .unwrap_or_else(|| r.session_id[..8.min(r.session_id.len())].to_string())
        } else {
            r.description.clone().unwrap_or_else(|| r.agent_id.clone())
        };
        let project = r
            .cwd
            .as_ref()
            .and_then(|c| projects.get(c).cloned())
            .unwrap_or_else(|| {
                r.cwd
                    .as_deref()
                    .map(|c| c.rsplit('/').next().unwrap_or(c).to_string())
                    .unwrap_or_default()
            });
        agents.push(Agent {
            seat: r.seat.clone(),
            session_id: r.session_id.clone(),
            agent_id: r.agent_id.clone(),
            parent: if r.agent_id.is_empty() {
                None
            } else {
                Some(r.parent_agent_id.clone().unwrap_or_default())
            },
            depth: r
                .depth
                .map(|d| d as u32)
                .unwrap_or(if r.agent_id.is_empty() { 0 } else { 1 }),
            name,
            agent_type: r
                .agent_type
                .clone()
                .or_else(|| (r.kind == "workflow").then(|| "workflow".to_string())),
            project,
            model: last_req.model.or_else(|| r.requested_model.clone()),
            effort: last_req.effort.or_else(|| r.effort.clone()),
            state,
            since,
            last_activity: last,
            cost,
            requests,
            context_tokens: last_req.context_tokens.unwrap_or(0).max(0) as u64,
            spark,
            lane: Vec::new(),
            tmux: live_session.and_then(|l| l.tmux.clone()),
            pid: live_session.map(|l| l.pid),
            account: last_req.account,
            first_seen: r.first_ts.map(from_ms).unwrap_or(last),
            session_cost: 0.0,
            session_burn: 0.0,
            cpu: 0.0,
            memory: 0,
            last_request_ts: req_ts,
        });
    }

    // Main threads waiting on their background agents.
    for i in 0..agents.len() {
        if agents[i].is_main() && agents[i].state == State::Idle {
            let n = agents
                .iter()
                .filter(|a| {
                    !a.is_main()
                        && a.seat == agents[i].seat
                        && a.session_id == agents[i].session_id
                        && a.state.is_active()
                })
                .count();
            if n > 0 {
                agents[i].state = State::WaitingAgents(n);
            }
        }
    }

    // Activity lanes: computed with the slow parts, carried over in between.
    match prev {
        Some(p) => {
            for a in agents.iter_mut() {
                if let Some(o) = p.agents.iter().find(|o| {
                    o.seat == a.seat && o.session_id == a.session_id && o.agent_id == a.agent_id
                }) {
                    a.lane = o.lane.clone();
                }
            }
        }
        None => {
            for a in agents.iter_mut() {
                a.lane = lane(idx, a, now).unwrap_or_default();
            }
        }
    }
    // Session totals for the main rows.
    if let Some(p) = prev {
        for a in agents.iter_mut().filter(|a| a.is_main()) {
            if let Some(o) = p
                .agents
                .iter()
                .find(|o| o.is_main() && o.seat == a.seat && o.session_id == a.session_id)
            {
                a.session_cost = o.session_cost.max(a.cost);
                a.session_burn = o.session_burn;
            }
            if let Some(n) = a.pid.and_then(|pid| procs.nodes.get(&pid)) {
                a.cpu = n.cpu;
                a.memory = n.memory;
            }
        }
    } else {
        let hour_ago = now - Duration::hours(1);
        for a in agents.iter_mut().filter(|a| a.is_main()) {
            (a.session_cost, a.session_burn) =
                idx.session_spend(&a.seat, &a.session_id, hour_ago)?;
            if let Some(n) = a.pid.and_then(|p| procs.nodes.get(&p)) {
                a.cpu = n.cpu;
                a.memory = n.memory;
            }
        }
    }
    let agents = order_forest(agents);
    let mut snap = Snapshot {
        taken_at: Some(now),
        agents,
        ..Default::default()
    };

    // Burn per minute by family, last 60 minutes.
    let start = now - Duration::minutes(60);
    let mut total = 0.0;
    for (i, model, c) in idx.spend_per_minute_by_model(start)? {
        if (0..60).contains(&i) {
            let fam = Family::of(&model).name().to_string();
            snap.burn.entry(fam).or_insert_with(|| vec![0.0; 60])[i as usize] += c;
            total += c;
        }
    }
    snap.burn_per_hour = total;
    let midnight = now
        .with_timezone(&chrono::Local)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let midnight = midnight
        .and_local_timezone(chrono::Local)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(now);
    snap.today_cost = idx.spend_since(midnight)?;
    // Recently finished agents (the history under the live tree).
    if let Some(p) = prev {
        snap.recent_done = p.recent_done.clone();
        snap.conflicts = p.conflicts.clone();
    } else {
        snap.conflicts = idx
            .edit_conflicts(now - Duration::minutes(30))
            .unwrap_or_default();
        for d in idx.recently_done(now - Duration::hours(6))? {
            let DoneAgent {
                name,
                cwd,
                first_ts: first,
                last_ts: last,
                cost,
                model,
            } = d;
            let project = cwd
                .as_ref()
                .and_then(|c| projects.get(c).cloned())
                .unwrap_or_else(|| {
                    cwd.as_deref()
                        .map(|c| c.rsplit('/').next().unwrap_or(c).to_string())
                        .unwrap_or_default()
                });
            let finished = last.map(from_ms).unwrap_or(now);
            snap.recent_done.push(DoneRow {
                name,
                project,
                model,
                started: first.map(from_ms).unwrap_or(finished),
                finished,
                cost: cost.unwrap_or(0.0),
            });
        }
    }
    snap.errors_last_hour = idx.api_error_counts(start)?;
    Ok(snap)
}

/// What happened before the dashboard started, so the event log is not empty on launch. The
/// dashboard words it (`PastEvent::kind` carries the facts, not text).
#[derive(Debug, Clone, PartialEq)]
pub struct PastEvent {
    pub at: DateTime<Utc>,
    pub kind: PastKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PastKind {
    Spawned {
        name: String,
        /// Short model name, empty when unknown.
        model: String,
        effort: Option<String>,
    },
    Done {
        name: String,
        took: Option<Duration>,
        cost: f64,
    },
    ApiError {
        kind: String,
        /// First line of the error text.
        text: Option<String>,
    },
    Prompt {
        /// Session title, empty when it has none.
        title: String,
        chars: i64,
    },
}

/// Agents spawned and finished, API errors and the user's prompts since `since`, newest first.
pub fn past_events(
    idx: &Index,
    since: DateTime<Utc>,
    limit: usize,
) -> anyhow::Result<Vec<PastEvent>> {
    let since_ms = ms(since);
    let mut out = Vec::new();
    for e in idx.agent_events(since)? {
        let model = e
            .model
            .as_deref()
            .map(crate::core::models::short_name)
            .unwrap_or_default();
        if let Some(f) = e.first_ts.filter(|f| *f >= since_ms) {
            out.push(PastEvent {
                at: from_ms(f),
                kind: PastKind::Spawned {
                    name: e.name.clone(),
                    model,
                    effort: e.effort.clone(),
                },
            });
        }
        if let (true, Some(l)) = (e.finished, e.last_ts.filter(|l| *l >= since_ms)) {
            out.push(PastEvent {
                at: from_ms(l),
                kind: PastKind::Done {
                    name: e.name,
                    took: e.first_ts.map(|f| from_ms(l) - from_ms(f)),
                    cost: e.cost.unwrap_or(0.0),
                },
            });
        }
    }
    for (ts, kind, text) in idx.api_errors_since(since)? {
        out.push(PastEvent {
            at: from_ms(ts),
            kind: PastKind::ApiError {
                kind,
                text: text.map(|t| t.lines().next().unwrap_or("").to_string()),
            },
        });
    }
    for (ts, chars, title) in idx.prompts_since(since)? {
        out.push(PastEvent {
            at: from_ms(ts),
            kind: PastKind::Prompt { title, chars },
        });
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.at));
    out.truncate(limit);
    Ok(out)
}

/// A stable tree: running sessions before exited ones, each in the order it started, followed by
/// its agents in spawn order. States change every second; positions must not, or the selection
/// and the reader's eye jump around.
fn order_forest(mut agents: Vec<Agent>) -> Vec<Agent> {
    agents.sort_by(|a, b| {
        (a.state == State::Gone)
            .cmp(&(b.state == State::Gone))
            .then(a.first_seen.cmp(&b.first_seen))
            .then(a.agent_id.cmp(&b.agent_id))
    });
    let mains: Vec<Agent> = agents.iter().filter(|a| a.is_main()).cloned().collect();
    let mut used = HashSet::new();
    let mut out = Vec::new();
    fn push_children(
        all: &[Agent],
        seat: &str,
        session: &str,
        parent: &str,
        depth: u32,
        used: &mut HashSet<String>,
        out: &mut Vec<Agent>,
    ) {
        for a in all.iter().filter(|a| {
            !a.is_main()
                && a.seat == seat
                && a.session_id == session
                && a.parent.as_deref().unwrap_or("") == parent
        }) {
            if used.insert(a.key()) {
                let mut a = a.clone();
                a.depth = depth;
                let id = a.agent_id.clone();
                out.push(a);
                push_children(all, seat, session, &id, depth + 1, used, out);
            }
        }
    }
    for m in &mains {
        used.insert(m.key());
        out.push(m.clone());
        push_children(&agents, &m.seat, &m.session_id, "", 1, &mut used, &mut out);
    }
    // Agents whose main thread is not shown (a session no longer running).
    for a in agents {
        if used.insert(a.key()) {
            out.push(a);
        }
    }
    out
}

/// The last text the agent wrote (read from the tail of its transcript).
pub fn last_message(idx: &Index, a: &Agent) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let path = idx.transcript_path(key_of(a)).ok().flatten()?;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(256 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    let mut events = Vec::new();
    let mut stats = crate::claude::transcript::ParseStats::default();
    // Skip the first, probably partial, line.
    for line in buf.lines().skip(if start > 0 { 1 } else { 0 }) {
        crate::claude::transcript::parse_line(line, &mut events, &mut stats);
    }
    events.iter().rev().find_map(|e| match e {
        crate::claude::transcript::Event::Request(r) => r.text.clone(),
        _ => None,
    })
}

/// The latest tool calls of one agent, newest first.
/// What an agent did during one minute, most telling first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize)]
pub enum Lane {
    #[default]
    Quiet,
    Thinking,
    Tool,
    /// Waiting for subagents (the `Agent` tool).
    Agents,
    /// Waiting for the user (a question or a plan).
    You,
}

pub const LANE_MINUTES: i64 = 30;

fn lane(idx: &Index, a: &Agent, now: DateTime<Utc>) -> anyhow::Result<Vec<Lane>> {
    let start = now - Duration::minutes(LANE_MINUTES);
    let mut out = vec![Lane::Quiet; LANE_MINUTES as usize];
    let slot = |t: i64| ((t - ms(start)) / 60_000).clamp(0, LANE_MINUTES - 1) as usize;
    for t in idx.agent_request_times(key_of(a), start)? {
        let i = slot(t);
        out[i] = out[i].max(Lane::Thinking);
    }
    for (t0, t1, name) in idx.agent_tool_spans(key_of(a), start, now)? {
        let kind = match ToolClass::of(&name) {
            ToolClass::AskUser | ToolClass::Plan => Lane::You,
            ToolClass::Agent => Lane::Agents,
            ToolClass::Edit | ToolClass::Read | ToolClass::Shell | ToolClass::Other => Lane::Tool,
        };
        for v in &mut out[slot(t0)..=slot(t1.max(t0))] {
            *v = (*v).max(kind);
        }
    }
    if matches!(a.state, State::NeedsYou(_)) {
        out[LANE_MINUTES as usize - 1] = Lane::You;
    }
    Ok(out)
}

/// One request of an agent: when, what it cost, how big the context was.
#[derive(Debug, Clone, Copy)]
pub struct LifePoint {
    pub ts: DateTime<Utc>,
    pub cost: f64,
    pub context: u64,
}

/// The agent's requests, oldest first (the last 2000).
pub fn life(idx: &Index, a: &Agent) -> anyhow::Result<Vec<LifePoint>> {
    let mut v: Vec<LifePoint> = idx
        .agent_requests(key_of(a), 2000)?
        .into_iter()
        .map(|r| LifePoint {
            ts: from_ms(r.ts),
            cost: r.cost,
            context: r.context.max(0) as u64,
        })
        .collect();
    v.reverse();
    Ok(v)
}

pub fn tool_timeline(idx: &Index, a: &Agent, limit: usize) -> anyhow::Result<Vec<ToolLine>> {
    Ok(idx
        .agent_tool_calls(key_of(a), limit)?
        .into_iter()
        .map(|t| ToolLine {
            ts: from_ms(t.ts),
            name: t.name,
            summary: t.summary.unwrap_or_default(),
            done: t.done_ts.map(from_ms),
            is_error: t.is_error,
        })
        .collect())
}

fn key_of(a: &Agent) -> AgentKey<'_> {
    AgentKey {
        seat: &a.seat,
        session_id: &a.session_id,
        agent_id: &a.agent_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, parent: Option<&str>, state: State) -> Agent {
        Agent {
            seat: "s".into(),
            session_id: "x".into(),
            agent_id: id.into(),
            parent: parent.map(str::to_string),
            depth: 0,
            name: id.into(),
            agent_type: None,
            project: "p".into(),
            model: None,
            effort: None,
            state,
            since: Utc::now(),
            last_activity: Utc::now(),
            cost: 0.0,
            requests: 0,
            context_tokens: 0,
            spark: vec![],
            tmux: None,
            pid: None,
            account: None,
            first_seen: Utc::now(),
            session_cost: 0.0,
            session_burn: 0.0,
            cpu: 0.0,
            memory: 0,
            last_request_ts: None,
            lane: Vec::new(),
        }
    }

    #[test]
    fn forest_order_nests_children_under_parents() {
        let v = vec![
            agent("b2", Some("b"), State::Thinking),
            agent("", None, State::Idle),
            agent("b", Some(""), State::Thinking),
            agent("c", Some(""), State::Done),
        ];
        let out = order_forest(v);
        let ids: Vec<(&str, u32)> = out.iter().map(|a| (a.agent_id.as_str(), a.depth)).collect();
        assert_eq!(ids, [("", 0), ("b", 1), ("b2", 2), ("c", 1)]);
    }
}
