//! The demo's Codex seat: a ChatGPT login, ten days of threads and one that is running, written
//! the way Codex 0.172 writes them (`session_meta`, `turn_context`, tool calls, each response's
//! usage as a `token_usage_record` and a `token_count`).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use super::{PROJECTS, append, cwd, rnd, ts};
use crate::claude::seats::Seat;

/// The demo's running Codex thread.
const LIVE: &str = "0d3c0de0-0000-4000-8000-0000000000d1";

const USER: &str = "user-demo-codex";
const WORKSPACE: &str = "ws-demo-codex";

/// What the demo's Codex calls: (tool, how it is called, argument).
const CALLS: &[(&str, &str, &str)] = &[
    ("shell", "function", "go test ./handlers/..."),
    (
        "apply_patch",
        "custom",
        "*** Begin Patch\n*** Update File: handlers/refund.go\n*** End Patch",
    ),
    ("shell", "function", "rg -n idempotency handlers"),
    ("shell", "function", "go vet ./..."),
];

fn line(t: DateTime<Utc>, kind: &str, payload: Value) -> Value {
    json!({"timestamp": ts(t), "type": kind, "payload": payload})
}

fn rollout(home: &Path, start: DateTime<Utc>, tid: &str) -> PathBuf {
    home.join("sessions")
        .join(start.format("%Y/%m/%d").to_string())
        .join(format!(
            "rollout-{}-{tid}.jsonl",
            start.format("%Y-%m-%dT%H-%M-%S")
        ))
}

fn tokens(input: u64, cached: u64, output: u64, reasoning: u64, total: u64) -> Value {
    json!({"input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
           "output_tokens": output, "reasoning_output_tokens": reasoning, "total_tokens": total})
}

/// One response: a tool call and its usage, written twice as Codex does; `total` is the
/// thread's running total.
fn response(seed: u64, t: DateTime<Utc>, tid: &str, total: &mut u64) -> Vec<Value> {
    let (name, how, arg) = CALLS[(seed % CALLS.len() as u64) as usize];
    let call_id = format!("call_{seed}");
    let call = if how == "custom" {
        json!({"type": "custom_tool_call", "name": name, "call_id": call_id, "input": arg})
    } else {
        json!({"type": "function_call", "name": name, "call_id": call_id,
               "arguments": json!({"command": ["bash", "-lc", arg]}).to_string()})
    };
    let cached = 30_000 + rnd(seed, 1) % 120_000;
    let input = cached + 1_500 + rnd(seed, 2) % 8_000;
    let output = 200 + rnd(seed, 3) % 1_800;
    let reasoning = output / 3;
    *total += input + output;
    let u = tokens(input, cached, output, reasoning, input + output);
    let t_all = tokens(0, 0, 0, 0, *total);
    vec![
        line(t, "response_item", call),
        line(
            t,
            "token_usage_record",
            json!({"thread_id": tid, "usage": u, "thread_token_usage": t_all}),
        ),
        line(
            t,
            "event_msg",
            json!({"type": "token_count", "info": {"total_token_usage": t_all, "last_token_usage": u}}),
        ),
    ]
}

fn output(seed: u64, t: DateTime<Utc>) -> Value {
    let kind = if CALLS[(seed % CALLS.len() as u64) as usize].1 == "custom" {
        "custom_tool_call_output"
    } else {
        "function_call_output"
    };
    line(
        t,
        "response_item",
        json!({"type": kind, "call_id": format!("call_{seed}"), "output": "ok"}),
    )
}

fn turn_start(t: DateTime<Utc>, model: &str, dir: &str, prompt: Option<&str>) -> Vec<Value> {
    let mut v = vec![
        line(t, "event_msg", json!({"type": "task_started"})),
        line(
            t,
            "turn_context",
            json!({"cwd": dir, "model": model, "effort": "medium"}),
        ),
    ];
    if let Some(p) = prompt {
        v.push(line(
            t,
            "event_msg",
            json!({"type": "item_completed", "item": {"type": "UserMessage",
                   "content": [{"type": "text", "text": p}]}}),
        ));
    }
    v
}

fn meta(t: DateTime<Utc>, tid: &str, dir: &str) -> Value {
    line(
        t,
        "session_meta",
        json!({"id": tid, "cwd": dir, "cli_version": "0.172.0", "originator": "codex-tui",
               "creator_user_id": USER, "creator_account_id": WORKSPACE, "git": {"branch": "main"}}),
    )
}

/// The Codex home: a ChatGPT login (unsigned demo tokens), named threads over ten days, and the
/// one that is running (its rollout is returned too).
pub(super) fn codex_seat(root: &Path, now: DateTime<Utc>) -> anyhow::Result<(Seat, PathBuf)> {
    let dir = root.join(".codex-demo");
    std::fs::create_dir_all(dir.join("thread-writer-locks"))?;
    let jwt = |claims: Value| {
        format!(
            "{}.{}.demo",
            crate::core::jwt::b64url(br#"{"alg":"none"}"#),
            crate::core::jwt::b64url(claims.to_string().as_bytes())
        )
    };
    let auth = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": jwt(json!({"email": "sam.demo@example.com", "name": "Sam Demo",
                "https://api.openai.com/auth": {"chatgpt_user_id": USER,
                    "chatgpt_account_id": WORKSPACE, "chatgpt_plan_type": "plus"}})),
            "access_token": jwt(json!({"exp": (now + Duration::days(9)).timestamp()})),
            "refresh_token": "demo-codex-refresh",
            "account_id": WORKSPACE,
        },
        "last_refresh": ts(now - Duration::hours(3)),
    });
    std::fs::write(dir.join("auth.json"), serde_json::to_string_pretty(&auth)?)?;
    let names = [
        "Refund idempotency",
        "Flaky checkout test",
        "Retry webhooks",
        "Trim training config",
        "Docs: API errors",
    ];
    let mut index = Vec::new();
    let mut live = PathBuf::new();
    for k in 0..8u64 {
        let (tid, project) = if k == 0 {
            (LIVE.to_string(), "payments-api")
        } else {
            (
                format!("0d3c0de0-0000-4000-8000-{:012x}", 0xd100 + k),
                PROJECTS[(k as usize + 1) % PROJECTS.len()],
            )
        };
        let start = if k == 0 {
            now - Duration::minutes(20)
        } else {
            now - Duration::days(k as i64) - Duration::hours((k * 3 % 7) as i64)
        };
        // Older threads ran GPT-5.6 Terra; the last days GPT-6 Luna, Codex's default.
        let model = if k < 4 { "gpt-6-luna" } else { "gpt-5.6-terra" };
        let name = names[(k as usize) % names.len()];
        let d = cwd(project);
        let mut lines = vec![meta(start, &tid, &d)];
        lines.extend(turn_start(start, model, &d, Some(name)));
        let (mut t, mut total) = (start, 0);
        for i in 0..(4 + k % 4) {
            let seed = 9_000_000 + k * 100 + i;
            t += Duration::seconds(30 + (rnd(k, i) % 60) as i64);
            lines.extend(response(seed, t, &tid, &mut total));
            lines.push(output(seed, t + Duration::seconds(3)));
        }
        if k > 0 {
            lines.push(line(
                t + Duration::seconds(5),
                "event_msg",
                json!({"type": "task_complete", "duration_ms": (t - start).num_milliseconds()}),
            ));
        }
        let path = rollout(&dir, start, &tid);
        append(&path, &lines);
        if k == 0 {
            live = path;
        }
        index.push(json!({"id": tid, "thread_name": name, "updated_at": ts(t)}));
    }
    let mut text = String::new();
    for v in index {
        text.push_str(&v.to_string());
        text.push('\n');
    }
    std::fs::write(dir.join("session_index.jsonl"), text)?;
    let seat = Seat {
        found_by: vec!["demo"],
        ..Seat::codex("codex", dir)
    };
    Ok((seat, live))
}

/// The running thread: its writer lock held while the demo runs, and a response now and then.
pub(super) struct Live {
    _lock: Option<std::fs::File>,
    path: PathBuf,
    dir: String,
    total: u64,
    open_call: Option<u64>,
}

impl Live {
    pub(super) fn start(home: &Path, path: PathBuf) -> Live {
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(
                home.join("thread-writer-locks")
                    .join(format!("{LIVE}.lock")),
            )
            .and_then(|f| f.lock().map(|()| f))
            .ok();
        Live {
            _lock: lock,
            path,
            dir: cwd("payments-api"),
            // Above anything the history wrote for this thread.
            total: 10_000_000,
            open_call: None,
        }
    }

    /// One step of the running thread: a turn starts, calls tools, ends, and starts again.
    pub(super) fn step(&mut self, now: DateTime<Utc>, tick: u64) {
        let mut lines = Vec::new();
        if let Some(seed) = self.open_call.take() {
            lines.push(output(seed, now));
        }
        let t = tick % 24;
        if t == 0 {
            lines.extend(turn_start(
                now,
                "gpt-6-luna",
                &self.dir,
                Some("Retry the refund webhook"),
            ));
        } else if t == 20 {
            lines.push(line(
                now,
                "event_msg",
                json!({"type": "task_complete", "duration_ms": 14_000}),
            ));
        } else if t < 20 && t % 3 == 1 {
            let seed = 8_000_000 + tick;
            lines.extend(response(seed, now, LIVE, &mut self.total));
            self.open_call = Some(seed);
        }
        if !lines.is_empty() {
            append(&self.path, &lines);
        }
    }
}
