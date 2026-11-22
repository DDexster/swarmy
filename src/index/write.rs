//! Writing what was read of one transcript file, inside the batch's transaction.

use rusqlite::params;

use super::{Work, ms, schema};
use crate::claude::files::{self, FileKind};
use crate::claude::transcript::{self, Event, TitleKind};

#[derive(Default)]
struct FileAgg {
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    cwd: Option<String>,
    title: Option<String>,
    ai_title: Option<String>,
}

/// Writes what was read of one file. Returns whether a request older than `settled_before`
/// (ms) was written: settled spend changed (see [`super::SETTLED_AFTER_MS`]).
pub(super) fn write_file(
    tx: &rusqlite::Transaction,
    w: &Work,
    chunk: transcript::Chunk,
    cwds: &mut std::collections::HashSet<String>,
    settled_before: i64,
) -> anyhow::Result<bool> {
    let mut late = false;
    let f = &w.file;
    let main = f.kind == FileKind::Main;
    if w.reset {
        // The file is read again from the start: forget the rows only it adds, so a file that
        // changed does not keep what it no longer says. Rows with keys (requests, tool calls,
        // prompts, errors, compactions) would not count twice anyway. Prompts and compactions
        // belong to the main transcript: a subagent's file never touches them.
        if main {
            tx.execute(
                "DELETE FROM prompts WHERE seat = ?1 AND session_id = ?2",
                params![f.seat, f.session_id],
            )?;
            tx.execute(
                "DELETE FROM compact_events WHERE seat = ?1 AND session_id = ?2",
                params![f.seat, f.session_id],
            )?;
        }
        tx.execute(
            "DELETE FROM api_errors WHERE seat = ?1 AND session_id = ?2 AND agent_id = ?3",
            params![f.seat, f.session_id, f.agent_id],
        )?;
        tx.execute(
            "DELETE FROM record_types WHERE path = ?1",
            [f.path.to_string_lossy()],
        )?;
    }
    // Exactly once per byte range: the offset is checked and advanced in this transaction.
    for (t, n) in &chunk.stats.by_type {
        tx.execute(
            "INSERT INTO record_types(path, type, count) VALUES (?1, ?2, ?3)
             ON CONFLICT(path, type) DO UPDATE SET count = count + excluded.count",
            params![f.path.to_string_lossy(), t, *n as i64],
        )?;
    }
    let mut account = w.account.clone();
    let mut last_ts = w.last_ts;
    let mut agg = FileAgg::default();
    let mut ins_req = tx.prepare_cached(&format!(
        "INSERT INTO requests(msg_id, seat, session_id, agent_id, account_uuid, ts, model, requested_model, effort, fast, cwd,
                              input, cw5, cw1h, cread, output, thinking, web)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(msg_id) DO UPDATE SET
           input = MAX(input, excluded.input), cw5 = MAX(cw5, excluded.cw5), cw1h = MAX(cw1h, excluded.cw1h),
           cread = MAX(cread, excluded.cread), output = MAX(output, excluded.output), thinking = MAX(thinking, excluded.thinking),
           web = MAX(web, excluded.web),
           account_uuid = COALESCE(account_uuid, excluded.account_uuid), {}",
        schema::unpriced_sql()
    ))?;
    let mut ins_tool = tx.prepare_cached(
        "INSERT INTO tool_calls(id, msg_id, seat, session_id, agent_id, ts, name, summary, target) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO NOTHING",
    )?;
    let mut upd_tool =
        tx.prepare_cached("UPDATE tool_calls SET done_ts = ?1, is_error = ?2 WHERE id = ?3")?;
    let mut count_tools = tx.prepare_cached(
        "UPDATE requests SET tools = (SELECT COUNT(*) FROM tool_calls WHERE msg_id = ?1) WHERE msg_id = ?1",
    )?;
    for ev in chunk.events {
        match ev {
            Event::Owner { account_uuid } => {
                tx.execute("INSERT INTO owners(seat, session_id, ts, account_uuid) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING", params![f.seat, f.session_id, last_ts, account_uuid])?;
                account = Some(account_uuid);
            }
            Event::Request(r) => {
                let t = ms(r.ts);
                late |= t < settled_before;
                last_ts = last_ts.max(t);
                agg.first_ts = Some(agg.first_ts.map_or(t, |x| x.min(t)));
                agg.last_ts = Some(agg.last_ts.map_or(t, |x| x.max(t)));
                if r.cwd.is_some() {
                    agg.cwd = r.cwd.clone();
                    if let Some(c) = &r.cwd
                        && !cwds.contains(c)
                    {
                        cwds.insert(c.clone());
                    }
                }
                let u = r.usage;
                ins_req.execute(params![
                    r.msg_id,
                    f.seat,
                    f.session_id,
                    f.agent_id,
                    if main { account.clone() } else { None },
                    t,
                    r.model,
                    r.requested_model,
                    r.effort,
                    r.fast as i64,
                    r.cwd,
                    u.input as i64,
                    u.cache_write_5m as i64,
                    u.cache_write_1h as i64,
                    u.cache_read as i64,
                    u.output as i64,
                    u.thinking as i64,
                    u.web_search as i64,
                ])?;
                for tu in &r.tool_uses {
                    if !tu.id.is_empty() {
                        ins_tool.execute(params![
                            tu.id,
                            r.msg_id,
                            f.seat,
                            f.session_id,
                            f.agent_id,
                            t,
                            tu.name,
                            tu.summary,
                            tu.target
                        ])?;
                    }
                }
                // A response's tool calls are counted, not added up: its records may be read
                // more than once, in any order.
                if !r.tool_uses.is_empty() {
                    count_tools.execute([&r.msg_id])?;
                }
            }
            Event::ToolResult {
                tool_use_id,
                ts,
                is_error,
            } => {
                if let Some(t) = ts {
                    last_ts = last_ts.max(ms(t));
                }
                upd_tool.execute(params![ts.map(ms), is_error as i64, tool_use_id])?;
            }
            Event::Prompt { ts, text } => {
                if let Some(t) = ts {
                    last_ts = last_ts.max(ms(t));
                    if main {
                        tx.execute("INSERT INTO prompts(seat, session_id, ts, chars) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING", params![f.seat, f.session_id, ms(t), text.chars().count() as i64])?;
                    }
                }
            }
            Event::Title { kind, text } => match kind {
                TitleKind::Custom => agg.title = Some(text),
                TitleKind::Ai => agg.ai_title = Some(text),
                TitleKind::AgentName => {
                    agg.title.get_or_insert(text);
                }
            },
            Event::TurnEnd { ts, .. } => {
                if let Some(t) = ts {
                    last_ts = last_ts.max(ms(t));
                }
            }
            Event::Compact { ts } => {
                if main {
                    let t = ts.map(ms).unwrap_or(last_ts);
                    tx.execute(
                        "INSERT INTO compact_events(seat, session_id, ts) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
                        params![f.seat, f.session_id, t],
                    )?;
                }
            }
            Event::AgentLaunch { .. } => {}
            Event::CostState { total_usd, models } => {
                // Cumulative per process; a resumed session starts a new count, so keep the largest.
                tx.execute(
                    "INSERT INTO cost_states(seat, session_id, total_usd, models) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(seat, session_id) DO UPDATE SET total_usd = excluded.total_usd, models = excluded.models
                     WHERE excluded.total_usd >= cost_states.total_usd",
                    params![f.seat, f.session_id, total_usd, serde_json::to_string(&models).unwrap_or_default()],
                )?;
            }
            Event::ApiError { ts, kind, text } => {
                let t = ts.map(ms).unwrap_or(last_ts);
                tx.execute(
                    "INSERT INTO api_errors(seat, session_id, agent_id, ts, kind, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT DO NOTHING",
                    params![f.seat, f.session_id, f.agent_id, t, kind, text],
                )?;
            }
        }
    }

    if main {
        if let Some(dir) = f.path.parent().map(|p| p.join(&f.session_id))
            && let Some(t) = files::read_custom_title(&dir)
        {
            agg.title = Some(t);
        }
        tx.execute(
            "INSERT INTO sessions(seat, session_id, title, ai_title, cwd, first_ts, last_ts, compactions)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, (SELECT COUNT(*) FROM compact_events WHERE seat = ?1 AND session_id = ?2))
             ON CONFLICT(seat, session_id) DO UPDATE SET
               title = COALESCE(excluded.title, title), ai_title = COALESCE(excluded.ai_title, ai_title), cwd = COALESCE(excluded.cwd, cwd),
               first_ts = MIN(COALESCE(first_ts, excluded.first_ts), COALESCE(excluded.first_ts, first_ts)),
               last_ts = MAX(COALESCE(last_ts, excluded.last_ts), COALESCE(excluded.last_ts, last_ts)),
               compactions = excluded.compactions",
            params![f.seat, f.session_id, agg.title, agg.ai_title, agg.cwd, agg.first_ts, agg.last_ts],
        )?;
    }
    let meta = f
        .meta_path()
        .and_then(|p| files::read_meta(&p))
        .unwrap_or_default();
    let workflow_run = match &f.kind {
        FileKind::Workflow(r) => Some(r.clone()),
        _ => None,
    };
    tx.execute(
        "INSERT INTO agents(seat, session_id, agent_id, kind, workflow_run, agent_type, description, tool_use_id, parent_agent_id, depth,
                            requested_model, effort, finished, first_ts, last_ts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(seat, session_id, agent_id) DO UPDATE SET
           agent_type = COALESCE(excluded.agent_type, agent_type), description = COALESCE(excluded.description, description),
           tool_use_id = COALESCE(excluded.tool_use_id, tool_use_id), parent_agent_id = COALESCE(excluded.parent_agent_id, parent_agent_id),
           depth = COALESCE(excluded.depth, depth), requested_model = COALESCE(excluded.requested_model, requested_model),
           effort = COALESCE(excluded.effort, effort), finished = excluded.finished,
           first_ts = MIN(COALESCE(first_ts, excluded.first_ts), COALESCE(excluded.first_ts, first_ts)),
           last_ts = MAX(COALESCE(last_ts, excluded.last_ts), COALESCE(excluded.last_ts, last_ts))",
        params![
            f.seat,
            f.session_id,
            f.agent_id,
            f.kind.tag(),
            workflow_run,
            meta.agent_type,
            meta.description,
            meta.tool_use_id,
            meta.parent_agent_id,
            meta.spawn_depth.map(|d| d as i64).or(if main { Some(0) } else { None }),
            meta.model,
            meta.effort,
            (meta.finished || main) as i64,
            agg.first_ts,
            agg.last_ts,
        ],
    )?;
    tx.execute(
        "INSERT INTO files(path, seat, session_id, agent_id, kind, ino, size, mtime, offset, last_ts, account_uuid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(path) DO UPDATE SET ino = excluded.ino, size = excluded.size, mtime = excluded.mtime, offset = excluded.offset,
           last_ts = excluded.last_ts, account_uuid = excluded.account_uuid",
        params![
            f.path.to_string_lossy(),
            f.seat,
            f.session_id,
            f.agent_id,
            f.kind.tag(),
            w.ino as i64,
            w.size as i64,
            w.mtime,
            chunk.end as i64,
            last_ts,
            account,
        ],
    )?;
    Ok(late)
}
