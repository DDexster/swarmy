//! The demo's Antigravity seat: a Google login, two weeks of Gemini conversations and one that
//! is running.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde_json::json;

use super::{PROJECTS, append, cwd, rnd, ts};
use crate::claude::seats::Seat;

/// The demo's running Antigravity conversation.
pub(super) const AGY_LIVE: &str = "7f3e2a10-demo-agy-api-guide";

/// What agy's demo steps call (tool, argument, value).
const AGY_TOOLS: &[(&str, &str, &str)] = &[
    (
        "view_file",
        "AbsolutePath",
        "/home/dev/work/docs-site/guide/api.md",
    ),
    ("grep_search", "Query", "rate limit"),
    (
        "replace_file_content",
        "TargetFile",
        "/home/dev/work/docs-site/guide/api.md",
    ),
    ("run_command", "CommandLine", "npm run build:docs"),
];

pub(super) fn agy_transcript(seat_dir: &Path, cid: &str) -> PathBuf {
    seat_dir
        .join("brain")
        .join(cid)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl")
}

/// One agy model step with a tool call, and the tool's result right after it.
pub(super) fn agy_planner(
    seed: u64,
    step: u64,
    t: DateTime<Utc>,
    model: &str,
    tool: u64,
) -> (serde_json::Value, serde_json::Value) {
    let (name, key, val) = AGY_TOOLS[(tool as usize) % AGY_TOOLS.len()];
    let done = t + Duration::seconds(2 + (seed % 5) as i64);
    (
        json!({"step_index": step, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE",
               "created_at": ts(t), "model": model, "input_tokens": 2_000 + rnd(seed, 1) % 9_000,
               "cache_read_tokens": 40_000 + rnd(seed, 2) % 160_000,
               "output_tokens": 150 + rnd(seed, 3) % 1_400, "content": "", "thinking": "…",
               "tool_calls": [{"name": name, "args": {key: val}}]}),
        json!({"step_index": step + 1, "source": "MODEL", "type": "GENERIC", "status": "DONE",
               "created_at": ts(done), "content": format!("Completed At: {}\nok", ts(done))}),
    )
}

/// The demo's Antigravity seat: a Google login, two weeks of Gemini conversations (the JSON
/// metadata agy keeps next to its database), and the one that is running.
pub(super) fn agy_seat(root: &Path, now: DateTime<Utc>) -> anyhow::Result<Seat> {
    let dir = root.join(".gemini-agy");
    for d in ["brain", "presence", "cache"] {
        std::fs::create_dir_all(dir.join(d))?;
    }
    std::fs::write(
        dir.join("antigravity-oauth-token"),
        json!({"token": {"access_token": "demo-agy-access", "token_type": "Bearer",
                         "refresh_token": "demo-agy-refresh", "expiry": ts(now + Duration::hours(1)),
                         "expires_in": 3599},
               "auth_method": "consumer", "id_token_email": "alex.demo@example.com"})
        .to_string(),
    )?;
    let mut convs = serde_json::Map::new();
    let titles = [
        "Rewrite the API guide",
        "Fix broken doc links",
        "Summarize refund flow",
        "Tidy pipeline configs",
        "Add search to docs",
    ];
    for k in 0..10u64 {
        let (cid, project, title) = if k == 0 {
            (AGY_LIVE.to_string(), "docs-site", titles[0])
        } else {
            (
                format!("{:08x}-demo-agy-{k}", 0x5eed_0000 + k),
                PROJECTS[(k as usize) % PROJECTS.len()],
                titles[(k as usize) % titles.len()],
            )
        };
        let start = if k == 0 {
            now - Duration::minutes(25)
        } else {
            now - Duration::days(k as i64) - Duration::hours((k * 5 % 9) as i64)
        };
        // Older conversations ran Gemini 3.1 Pro; the last few days Gemini 4 Neon.
        let model = if k < 5 {
            "gemini-4-neon-high"
        } else {
            "gemini-3.1-pro-preview-high"
        };
        let mut lines = vec![
            json!({"step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT",
            "status": "DONE", "created_at": ts(start), "content": format!("<USER_REQUEST>\n{title}\n</USER_REQUEST>")}),
        ];
        let mut t = start;
        for i in 0..(6 + k % 5) {
            t += Duration::seconds(40 + (rnd(k, i) % 80) as i64);
            let (p, g) = agy_planner(7_000_000 + k * 100 + i, 1 + 2 * i, t, model, k + i);
            lines.extend([p, g]);
        }
        append(&agy_transcript(&dir, &cid), &lines);
        convs.insert(
            cid.clone(),
            json!({"summary": {"ID": cid, "Title": title,
                               "WorkspaceURIs": [format!("file://{}", cwd(project))],
                               "UpdatedAt": ts(t)}}),
        );
    }
    std::fs::write(
        dir.join("cache").join("conversation_metadata.json"),
        json!({"conversations": convs}).to_string(),
    )?;
    std::fs::write(dir.join("presence").join(format!("{AGY_LIVE}.lock")), "")?;
    Ok(Seat {
        found_by: vec!["demo"],
        ..Seat::agy("agy", dir)
    })
}
