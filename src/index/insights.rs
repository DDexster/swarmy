//! Efficiency figures for a period: where agents spend their time, how long they wait for the
//! user, what the cache saves, which agent types cost what, how big the fleet gets.

use std::collections::HashMap;

use rusqlite::params;

use super::{Index, ms};
use crate::core::period::Span;
use crate::core::pricing::PriceBook;

#[derive(Debug, Clone, Default)]
pub struct ToolStat {
    pub name: String,
    pub calls: u64,
    pub errors: u64,
    /// Agent-hours spent inside the tool.
    pub hours: f64,
}

#[derive(Debug, Clone, Default)]
pub struct TypeStat {
    pub name: String,
    pub agents: u64,
    pub cost: f64,
    /// Average lifetime, minutes.
    pub avg_minutes: f64,
    /// Subagents that never reported finishing (killed, stalled, still running).
    pub unfinished: u64,
}

/// Dollars by kind of token.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokenSplit {
    pub input: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub output: f64,
}

impl TokenSplit {
    pub fn total(&self) -> f64 {
        self.input + self.cache_write + self.cache_read + self.output
    }
}

#[derive(Debug, Clone, Default)]
pub struct Efficiency {
    pub tools: Vec<ToolStat>,
    /// Bash commands grouped by program (`cargo test`, `flutter`, …), by time.
    pub commands: Vec<ToolStat>,
    /// Questions to the user: count and hours the agents waited for the answer.
    pub questions: (u64, f64),
    /// Plans waiting for approval.
    pub plans: (u64, f64),
    pub prompts: u64,
    /// Lifetime of all agents that ran in the period, hours.
    pub agent_hours: f64,
    pub cost: f64,
    pub cache_hit: f64,
    /// What cache reads saved against paying the input rate.
    pub cache_saved: f64,
    pub split: TokenSplit,
    pub types: Vec<TypeStat>,
    /// Agents active in each of the last 24 hours (oldest first).
    pub fleet: Vec<u32>,
    pub rate_limits: u64,
    pub other_errors: u64,
    pub compactions: u64,
    pub files_edited: u64,
    pub edits: u64,
    /// Files edited by more than one agent within the same hour, last 24 hours.
    pub conflicts: Vec<(String, u32)>,
}

/// `cd x && FOO=1 cargo test -p y | tail` → `cargo test`; `tools/agents/gate.sh` → `gate.sh`.
pub fn command_key(cmd: &str) -> String {
    const SKIP: &[&str] = &[
        "cd", "export", "set", "source", ".", "mkdir", "echo", "sleep", "true", "unset", "trap",
        "local", "rm",
    ];
    const TWO: &[&str] = &[
        "cargo", "flutter", "dart", "npm", "npx", "pnpm", "yarn", "make", "git", "gh", "docker",
        "go", "pip", "uv", "node",
    ];
    let mut fallback = None;
    for seg in cmd.split(['\n', ';', '|', '&']) {
        let mut words = seg.split_whitespace().skip_while(|w| {
            w.contains('=') && !w.starts_with('-') && !w.starts_with('"')
                || matches!(
                    *w,
                    "env" | "time" | "do" | "then" | "else" | "{" | "(" | "!" | "exec" | "nice"
                )
        });
        let Some(first) = words.next() else { continue };
        let prog = first
            .rsplit('/')
            .next()
            .unwrap_or(first)
            .trim_matches(['(', ')', '"', '\'']);
        if prog.is_empty() || prog.starts_with('$') || matches!(prog, "done" | "fi" | "esac" | "}")
        {
            continue;
        }
        if SKIP.contains(&prog)
            || prog.starts_with("for")
            || prog.starts_with("while")
            || prog.starts_with("if")
        {
            fallback.get_or_insert_with(|| prog.to_string());
            continue;
        }
        if TWO.contains(&prog)
            && let Some(sub) = words.find(|w| !w.starts_with('-') && !w.starts_with('+'))
        {
            return format!("{prog} {sub}");
        }
        return prog.to_string();
    }
    fallback.unwrap_or_else(|| "?".into())
}

impl Index {
    pub fn efficiency(&self, span: Span, prices: &PriceBook) -> anyhow::Result<Efficiency> {
        let c = &self.conn;
        let (a, b) = (ms(span.start), ms(span.end));
        let mut e = Efficiency::default();
        let t0 = std::time::Instant::now();
        let dbg = std::env::var_os("AGENTMANA_DEBUG").is_some();
        let mark = |n: &str| {
            if dbg {
                eprintln!("    eff {n} {:?}", t0.elapsed());
            }
        };

        // Tools by time; Bash also by program.
        let mut st = c.prepare(
            "SELECT name, COUNT(*), SUM(is_error), SUM(CASE WHEN done_ts >= ts THEN done_ts - ts ELSE 0 END)
             FROM tool_calls WHERE ts >= ?1 AND ts < ?2 GROUP BY name",
        )?;
        for row in st.query_map(params![a, b], |r| {
            Ok(ToolStat {
                name: r.get(0)?,
                calls: r.get::<_, i64>(1)? as u64,
                errors: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                hours: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as f64 / 3.6e6,
            })
        })? {
            let t = row?;
            match t.name.as_str() {
                "AskUserQuestion" => {
                    e.questions = (e.questions.0 + t.calls, e.questions.1 + t.hours)
                }
                "ExitPlanMode" => e.plans = (e.plans.0 + t.calls, e.plans.1 + t.hours),
                _ => {}
            }
            e.tools.push(t);
        }
        e.tools
            .sort_by(|x, y| y.hours.total_cmp(&x.hours).then(y.calls.cmp(&x.calls)));

        mark("tools");
        let mut cmds: HashMap<String, ToolStat> = HashMap::new();
        let mut st = c.prepare(
            "SELECT summary, done_ts - ts, is_error FROM tool_calls
             WHERE name = 'Bash' AND ts >= ?1 AND ts < ?2 AND summary IS NOT NULL",
        )?;
        for row in st.query_map(params![a, b], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })? {
            let (cmd, dur, err) = row?;
            let k = command_key(&cmd);
            let s = cmds.entry(k.clone()).or_insert_with(|| ToolStat {
                name: k,
                ..Default::default()
            });
            s.calls += 1;
            s.errors += err as u64;
            s.hours += dur.unwrap_or(0).max(0) as f64 / 3.6e6;
        }
        e.commands = cmds.into_values().collect();
        e.commands.sort_by(|x, y| y.hours.total_cmp(&x.hours));
        e.commands.truncate(12);

        mark("commands");
        e.prompts = c.query_row(
            "SELECT COUNT(*) FROM prompts WHERE ts >= ?1 AND ts < ?2",
            params![a, b],
            |r| r.get::<_, i64>(0),
        )? as u64;

        // Tokens and dollars by kind, per model (prices differ).
        let mut st = c.prepare(
            "SELECT model, SUM(input), SUM(cw5), SUM(cw1h), SUM(cread), SUM(output), SUM(MAX(cost, 0))
             FROM requests WHERE ts >= ?1 AND ts < ?2 GROUP BY model",
        )?;
        let day = span.end.date_naive();
        let (mut fresh, mut read) = (0f64, 0f64);
        for row in st.query_map(params![a, b], |r| {
            Ok((
                r.get::<_, String>(0)?,
                [1, 2, 3, 4, 5]
                    .map(|i| r.get::<_, Option<i64>>(i).ok().flatten().unwrap_or(0) as f64),
                r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
            ))
        })? {
            let (model, [input, cw5, cw1h, cread, output], cost) = row?;
            e.cost += cost;
            fresh += input + cw5 + cw1h;
            read += cread;
            if let Some(p) = prices.lookup(&model, day) {
                let r = &p.rates;
                let m = 1e-6;
                e.split.input += input * r.input * m;
                e.split.cache_write += (cw5 * r.cache_write_5m + cw1h * r.cache_write_1h) * m;
                e.split.cache_read += cread * r.cache_read * m;
                e.split.output += output * r.output * m;
                e.cache_saved += cread * (r.input - r.cache_read).max(0.0) * m;
            }
        }
        e.cache_hit = if fresh + read > 0.0 {
            read / (fresh + read)
        } else {
            0.0
        };

        mark("tokens");
        // Agent types (main threads count as "main").
        let mut st = c.prepare(
            "SELECT CASE WHEN a.kind = 'main' THEN 'main thread' ELSE COALESCE(a.agent_type, a.kind) END,
                    COUNT(*), SUM(CASE WHEN a.kind != 'main' AND a.finished = 0 THEN 1 ELSE 0 END),
                    AVG(a.last_ts - a.first_ts), SUM(a.last_ts - a.first_ts),
                    SUM((SELECT SUM(MAX(r.cost, 0)) FROM requests r WHERE r.seat = a.seat AND r.session_id = a.session_id
                         AND r.agent_id = a.agent_id AND r.ts >= ?1 AND r.ts < ?2))
             FROM agents a WHERE a.last_ts >= ?1 AND a.first_ts < ?2 GROUP BY 1",
        )?;
        for row in st.query_map(params![a, b], |r| {
            Ok((
                TypeStat {
                    name: r.get(0)?,
                    agents: r.get::<_, i64>(1)? as u64,
                    unfinished: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                    avg_minutes: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0) / 60_000.0,
                    cost: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                },
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            ))
        })? {
            let (t, life) = row?;
            if t.name != "main thread" {
                e.agent_hours += life.max(0) as f64 / 3.6e6;
            }
            e.types.push(t);
        }
        e.types.sort_by(|x, y| y.cost.total_cmp(&x.cost));

        mark("types");
        // The fleet over the last 24 hours.
        let now = chrono::Utc::now();
        let h0 = ms(now - chrono::Duration::hours(24)) / 3_600_000;
        e.fleet = vec![0; 25];
        let mut st = c.prepare(
            "SELECT ts / 3600000, COUNT(DISTINCT seat || session_id || agent_id) FROM requests WHERE ts >= ?1 GROUP BY 1",
        )?;
        for row in st.query_map(params![h0 * 3_600_000], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (h, n) = row?;
            if let Some(slot) = e.fleet.get_mut((h - h0) as usize) {
                *slot = n as u32;
            }
        }

        let mut st = c.prepare(
            "SELECT kind, COUNT(*) FROM api_errors WHERE ts >= ?1 AND ts < ?2 GROUP BY 1",
        )?;
        for row in st.query_map(params![a, b], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (k, n) = row?;
            if k == "rate_limit" {
                e.rate_limits += n as u64;
            } else {
                e.other_errors += n as u64;
            }
        }
        e.compactions = c.query_row(
            "SELECT COALESCE(SUM(compactions), 0) FROM sessions WHERE last_ts >= ?1 AND first_ts < ?2",
            params![a, b],
            |r| r.get::<_, i64>(0),
        )? as u64;

        mark("fleet+errors");
        let (files, edits): (i64, i64) = c.query_row(
            "SELECT COUNT(DISTINCT target), COUNT(*) FROM tool_calls
             WHERE name IN ('Edit', 'Write', 'MultiEdit', 'NotebookEdit') AND ts >= ?1 AND ts < ?2",
            params![a, b],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        (e.files_edited, e.edits) = (files as u64, edits as u64);
        mark("files");
        e.conflicts = self.edit_conflicts(now - chrono::Duration::hours(24))?;
        mark("conflicts");
        Ok(e)
    }

    /// Files edited by more than one agent within the same hour since `since`, most agents first.
    pub fn edit_conflicts(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<(String, u32)>> {
        let mut st = self.conn.prepare(
            "SELECT target, MAX(n) FROM (
               SELECT target, ts / 3600000, COUNT(DISTINCT seat || session_id || agent_id) n FROM tool_calls
               WHERE name IN ('Edit', 'Write', 'MultiEdit') AND ts >= ?1 AND target IS NOT NULL GROUP BY 1, 2)
             WHERE n > 1 GROUP BY 1 ORDER BY 2 DESC LIMIT 20",
        )?;
        let rows = st.query_map(params![ms(since)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

impl Index {
    /// Spend per local day and model (for stacked daily bars).
    pub fn days_by_model(&self, span: Span) -> anyhow::Result<Vec<(String, String, f64)>> {
        let mut st = self.conn.prepare(
            "SELECT ts / 3600000, model, SUM(MAX(cost, 0)) FROM requests WHERE ts >= ?1 AND ts < ?2 GROUP BY 1, 2",
        )?;
        let mut acc: std::collections::BTreeMap<(String, String), f64> = Default::default();
        for row in st.query_map(params![ms(span.start), ms(span.end)], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            ))
        })? {
            let (h, model, c) = row?;
            let day = super::from_ms(h * 3_600_000)
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string();
            *acc.entry((day, model)).or_default() += c;
        }
        Ok(acc.into_iter().map(|((d, m), c)| (d, m, c)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::command_key;

    #[test]
    fn commands_are_grouped_by_program() {
        assert_eq!(
            command_key("cd ~/x && cargo test -p foo --no-fail-fast 2>&1 | tail"),
            "cargo test"
        );
        assert_eq!(
            command_key("export TMPDIR=$HOME/tmp; RUST_LOG=1 cargo +nightly build"),
            "cargo build"
        );
        assert_eq!(
            command_key("tools/agents/cargo-heavy.sh tools/agents/gate.sh"),
            "cargo-heavy.sh"
        );
        assert_eq!(command_key("git -C repo status --short"), "git repo");
        assert_eq!(command_key("python3 - <<'EOF'"), "python3");
        assert_eq!(command_key("sleep 30"), "sleep");
        assert_eq!(command_key("for i in 1 2; do grep -n x f; done"), "grep");
    }
}
