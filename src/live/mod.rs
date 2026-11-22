//! The live state of every agent: who is thinking, running a tool, waiting for the human,
//! waiting for its children, stalled or done. Built from the index (freshly refreshed), the
//! session registry and the process table.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use rusqlite::params;
use serde::Serialize;

use crate::claude::registry::{LiveSession, SessionStatus};
use crate::core::models::Family;
use crate::index::{Index, from_ms, ms};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum State {
    /// Blocked on the human (permission, question, dialog).
    NeedsYou(String),
    /// Running a tool.
    Tool {
        name: String,
        summary: String,
    },
    /// Waiting for the model.
    Thinking,
    /// Idle main thread with background agents still running.
    WaitingAgents(usize),
    Idle,
    /// Unfinished, no transcript activity for a while.
    Stalled,
    Done,
    /// The Claude Code process is gone.
    Gone,
}

impl State {
    pub fn rank(&self) -> u8 {
        match self {
            State::NeedsYou(_) => 0,
            State::Stalled => 1,
            State::Tool { .. } => 2,
            State::Thinking => 3,
            State::WaitingAgents(_) => 4,
            State::Idle => 5,
            State::Done => 6,
            State::Gone => 7,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            State::Tool { .. } | State::Thinking | State::WaitingAgents(_)
        )
    }
}

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
    pub tmux: Option<String>,
    pub pid: Option<u32>,
    pub account: Option<String>,
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
}

impl Snapshot {
    pub fn count(&self, f: impl Fn(&State) -> bool) -> usize {
        self.agents.iter().filter(|a| f(&a.state)).count()
    }
}

pub struct Options {
    pub stall_minutes: i64,
    /// Finished agents stay on screen this long.
    pub linger_minutes: i64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            stall_minutes: 20,
            linger_minutes: 3,
        }
    }
}

struct Row {
    seat: String,
    session_id: String,
    agent_id: String,
    kind: String,
    agent_type: Option<String>,
    description: Option<String>,
    parent_agent_id: Option<String>,
    depth: Option<i64>,
    requested_model: Option<String>,
    effort: Option<String>,
    finished: bool,
    last_ts: Option<i64>,
    title: Option<String>,
    cwd: Option<String>,
}

pub fn build(
    idx: &Index,
    live: &[LiveSession],
    opts: &Options,
    now: DateTime<Utc>,
) -> anyhow::Result<Snapshot> {
    let conn = &idx.conn;
    let recent = ms(now - Duration::minutes(opts.stall_minutes.max(30)));
    let unfinished_horizon = ms(now - Duration::hours(24));
    let live_keys: HashSet<(String, String)> = live
        .iter()
        .map(|l| (l.seat.clone(), l.session_id.clone()))
        .collect();

    let mut rows: Vec<Row> = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT a.seat, a.session_id, a.agent_id, a.kind, a.agent_type, a.description, a.parent_agent_id, a.depth,
                    a.requested_model, a.effort, a.finished, a.last_ts, COALESCE(s.title, s.ai_title), s.cwd
             FROM agents a LEFT JOIN sessions s ON s.seat = a.seat AND s.session_id = a.session_id
             WHERE a.last_ts >= ?1 OR (a.finished = 0 AND a.last_ts >= ?2) OR a.agent_id = ''",
        )?;
        let it = st.query_map(params![recent, unfinished_horizon], |r| {
            Ok(Row {
                seat: r.get(0)?,
                session_id: r.get(1)?,
                agent_id: r.get(2)?,
                kind: r.get(3)?,
                agent_type: r.get(4)?,
                description: r.get(5)?,
                parent_agent_id: r.get(6)?,
                depth: r.get(7)?,
                requested_model: r.get(8)?,
                effort: r.get(9)?,
                finished: r.get::<_, i64>(10)? != 0,
                last_ts: r.get(11)?,
                title: r.get(12)?,
                cwd: r.get(13)?,
            })
        })?;
        for row in it {
            let row = row?;
            let is_live_main = row.agent_id.is_empty()
                && live_keys.contains(&(row.seat.clone(), row.session_id.clone()));
            let recent_enough = row.last_ts.is_some_and(|t| t >= recent);
            if row.agent_id.is_empty() && !is_live_main && !recent_enough {
                continue;
            }
            rows.push(row);
        }
    }
    // Live sessions that have no transcript yet still show up.
    for l in live {
        if !rows
            .iter()
            .any(|r| r.agent_id.is_empty() && r.seat == l.seat && r.session_id == l.session_id)
        {
            rows.push(Row {
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
                title: l.name.clone(),
                cwd: Some(l.cwd.clone()),
            });
        }
    }
    let projects: HashMap<String, String> = {
        let mut st = conn.prepare("SELECT cwd, name FROM projects")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?
    };

    let mut last_req = conn.prepare(
        "SELECT model, effort, ts, input + cw5 + cw1h + cread, account_uuid FROM requests
         WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3 ORDER BY ts DESC LIMIT 1",
    )?;
    let mut totals = conn.prepare("SELECT COUNT(*), SUM(MAX(cost, 0)) FROM requests WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3")?;
    let mut open_tool = conn.prepare(
        "SELECT name, summary, ts FROM tool_calls WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3 AND done_ts IS NULL AND ts >= ?4
         ORDER BY ts DESC LIMIT 1",
    )?;
    let mut last_done = conn.prepare(
        "SELECT MAX(done_ts) FROM tool_calls WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3",
    )?;
    let mut spark = conn.prepare(
        "SELECT (ts - ?4) / 60000, SUM(MAX(cost, 0)) FROM requests WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3 AND ts >= ?4 GROUP BY 1",
    )?;
    let spark_start = ms(now - Duration::minutes(15));

    let mut agents = Vec::new();
    for r in &rows {
        let (model, effort, req_ts, ctx_tokens, account) = last_req
            .query_row(params![r.seat, r.session_id, r.agent_id], |x| {
                Ok((
                    x.get::<_, Option<String>>(0)?,
                    x.get::<_, Option<String>>(1)?,
                    x.get::<_, Option<i64>>(2)?,
                    x.get::<_, Option<i64>>(3)?,
                    x.get::<_, Option<String>>(4)?,
                ))
            })
            .unwrap_or((None, None, None, None, None));
        let (requests, cost): (i64, Option<f64>) = totals
            .query_row(params![r.seat, r.session_id, r.agent_id], |x| {
                Ok((x.get(0)?, x.get(1)?))
            })?;
        let tool = match req_ts {
            Some(t) => open_tool
                .query_row(params![r.seat, r.session_id, r.agent_id, t], |x| {
                    Ok((
                        x.get::<_, String>(0)?,
                        x.get::<_, Option<String>>(1)?,
                        x.get::<_, i64>(2)?,
                    ))
                })
                .ok(),
            None => None,
        };
        let done_ts: Option<i64> = last_done
            .query_row(params![r.seat, r.session_id, r.agent_id], |x| x.get(0))
            .unwrap_or(None);
        let mut sp = vec![0.0; 15];
        let it = spark.query_map(
            params![r.seat, r.session_id, r.agent_id, spark_start],
            |x| {
                Ok((
                    x.get::<_, i64>(0)?,
                    x.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                ))
            },
        )?;
        for row in it {
            let (i, c) = row?;
            if (0..15).contains(&i) {
                sp[i as usize] = c;
            }
        }
        let last = [r.last_ts, req_ts, done_ts]
            .into_iter()
            .flatten()
            .max()
            .map(from_ms)
            .unwrap_or(now);
        let live_session = live
            .iter()
            .find(|l| l.seat == r.seat && l.session_id == r.session_id);
        let silent = now - last;

        let (state, since) = if r.agent_id.is_empty() {
            match live_session.map(|l| &l.status) {
                None => (State::Gone, last),
                Some(SessionStatus::Waiting(w)) => (
                    State::NeedsYou(w.clone()),
                    live_session.and_then(|l| l.status_since).unwrap_or(last),
                ),
                Some(SessionStatus::Busy) => match &tool {
                    Some((n, s, t)) => (
                        State::Tool {
                            name: n.clone(),
                            summary: s.clone().unwrap_or_default(),
                        },
                        from_ms(*t),
                    ),
                    None => (
                        State::Thinking,
                        live_session.and_then(|l| l.status_since).unwrap_or(last),
                    ),
                },
                Some(_) => (
                    State::Idle,
                    live_session.and_then(|l| l.status_since).unwrap_or(last),
                ),
            }
        } else if r.finished {
            (State::Done, last)
        } else if !live_keys.contains(&(r.seat.clone(), r.session_id.clone()))
            || silent > Duration::minutes(opts.stall_minutes)
        {
            (State::Stalled, last)
        } else {
            match &tool {
                Some((n, s, t)) => (
                    State::Tool {
                        name: n.clone(),
                        summary: s.clone().unwrap_or_default(),
                    },
                    from_ms(*t),
                ),
                None => (State::Thinking, last),
            }
        };
        if state == State::Done && silent > Duration::minutes(opts.linger_minutes) {
            continue;
        }
        if state == State::Stalled && silent > Duration::hours(24) {
            continue;
        }
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
            model: model.or_else(|| r.requested_model.clone()),
            effort: effort.or_else(|| r.effort.clone()),
            state,
            since,
            last_activity: last,
            cost: cost.unwrap_or(0.0),
            requests: requests as u64,
            context_tokens: ctx_tokens.unwrap_or(0) as u64,
            spark: sp,
            tmux: live_session.and_then(|l| l.tmux.clone()),
            pid: live_session.map(|l| l.pid),
            account,
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

    let agents = order_forest(agents);
    let mut snap = Snapshot {
        taken_at: Some(now),
        agents,
        ..Default::default()
    };

    // Burn per minute by family, last 60 minutes.
    let start = ms(now - Duration::minutes(60));
    let mut st = conn.prepare("SELECT (ts - ?1) / 60000, model, SUM(MAX(cost, 0)) FROM requests WHERE ts >= ?1 GROUP BY 1, 2")?;
    let it = st.query_map(params![start], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
        ))
    })?;
    let mut total = 0.0;
    for row in it {
        let (i, model, c) = row?;
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
    snap.today_cost = conn
        .query_row(
            "SELECT SUM(MAX(cost, 0)) FROM requests WHERE ts >= ?1",
            params![ms(midnight)],
            |r| r.get::<_, Option<f64>>(0),
        )?
        .unwrap_or(0.0);
    let mut st = conn.prepare("SELECT kind, COUNT(*) FROM api_errors WHERE ts >= ?1 GROUP BY 1")?;
    snap.errors_last_hour = st
        .query_map(params![start], |r| {
            Ok((r.get(0)?, r.get::<_, i64>(1)? as u64))
        })?
        .collect::<Result<_, _>>()?;
    Ok(snap)
}

/// Main threads by state then activity; each followed by its agents as a tree.
fn order_forest(mut agents: Vec<Agent>) -> Vec<Agent> {
    agents.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then(b.last_activity.cmp(&a.last_activity))
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

/// The latest tool calls of one agent, newest first.
pub fn tool_timeline(idx: &Index, a: &Agent, limit: usize) -> anyhow::Result<Vec<ToolLine>> {
    let mut st = idx.conn.prepare(
        "SELECT ts, name, summary, done_ts, is_error FROM tool_calls WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3 ORDER BY ts DESC LIMIT ?4",
    )?;
    let it = st.query_map(
        params![a.seat, a.session_id, a.agent_id, limit as i64],
        |r| {
            Ok(ToolLine {
                ts: from_ms(r.get(0)?),
                name: r.get(1)?,
                summary: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                done: r.get::<_, Option<i64>>(3)?.map(from_ms),
                is_error: r.get::<_, i64>(4)? != 0,
            })
        },
    )?;
    Ok(it.collect::<Result<_, _>>()?)
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
