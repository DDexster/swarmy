//! Read queries over the index.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rusqlite::params_from_iter;
use serde::Serialize;

use super::{Index, from_ms, ms};
use crate::core::limits::{Reading, WindowKind};
use crate::core::period::{BUCKET_MS, Span, bucket_start};
use crate::core::usage::Usage;

/// The label of an agent's type over `agents a`, the same in `report --by agent-type` and the
/// EFFICIENCY panel: "main thread", "workflow:<type>", or the subagent's type.
macro_rules! agent_type_label {
    () => {
        "CASE WHEN a.kind = 'main' THEN 'main thread' WHEN a.kind = 'workflow' THEN 'workflow:' || COALESCE(a.agent_type, '?') ELSE COALESCE(a.agent_type, 'subagent') END"
    };
}
pub(super) const AGENT_TYPE: &str = agent_type_label!();
/// [`AGENT_TYPE`] of a request (`requests r` joined with `agents a`, which may have no row).
const AGENT_TYPE_OF_REQUEST: &str = concat!(
    "CASE WHEN r.agent_id = '' THEN 'main thread' ELSE ",
    agent_type_label!(),
    " END"
);

/// A grouping dimension for spend reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
pub enum Dim {
    Project,
    Account,
    Seat,
    Model,
    Effort,
    AgentType,
    Session,
    Agent,
    Day,
    Hour,
    Weekday,
}

impl Dim {
    /// (key expression, label expression) over `requests r`, `projects p`, `agents a`,
    /// `sessions s`, `accounts c`.
    fn exprs(self) -> (&'static str, &'static str) {
        match self {
            Dim::Project => (
                "COALESCE(p.key, r.cwd, '?')",
                "MAX(COALESCE(p.name, r.cwd, '(unknown)'))",
            ),
            Dim::Account => (
                "COALESCE(r.account_uuid, '?')",
                "MAX(COALESCE(c.email, c.display_name, r.account_uuid, '(unknown)'))",
            ),
            Dim::Seat => ("r.seat", "r.seat"),
            Dim::Model => ("r.model", "r.model"),
            Dim::Effort => ("COALESCE(r.effort, '-')", "COALESCE(r.effort, '-')"),
            Dim::AgentType => (AGENT_TYPE_OF_REQUEST, AGENT_TYPE_OF_REQUEST),
            Dim::Session => (
                "r.seat || '/' || r.session_id",
                "MAX(COALESCE(s.title, s.ai_title, substr(r.session_id, 1, 8)))",
            ),
            Dim::Agent => (
                "r.seat || '/' || r.session_id || '/' || r.agent_id",
                "MAX(CASE WHEN r.agent_id = '' THEN COALESCE(s.title, s.ai_title, substr(r.session_id, 1, 8)) || ' (main)' ELSE COALESCE(a.description, r.agent_id) END)",
            ),
            Dim::Day => (
                "date(r.ts / 1000, 'unixepoch', 'localtime')",
                "date(r.ts / 1000, 'unixepoch', 'localtime')",
            ),
            Dim::Hour => (
                "strftime('%H', r.ts / 1000, 'unixepoch', 'localtime')",
                "strftime('%H', r.ts / 1000, 'unixepoch', 'localtime')",
            ),
            Dim::Weekday => (
                "strftime('%w', r.ts / 1000, 'unixepoch', 'localtime')",
                "strftime('%w', r.ts / 1000, 'unixepoch', 'localtime')",
            ),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Dim::Project => "PROJECT",
            Dim::Account => "ACCOUNT",
            Dim::Seat => "SEAT",
            Dim::Model => "MODEL",
            Dim::Effort => "EFFORT",
            Dim::AgentType => "AGENT TYPE",
            Dim::Session => "SESSION",
            Dim::Agent => "AGENT",
            Dim::Day => "DAY",
            Dim::Hour => "HOUR",
            Dim::Weekday => "WEEKDAY",
        }
    }

    /// Chronological dimensions sort by key, the others by cost.
    pub fn is_time(self) -> bool {
        matches!(self, Dim::Day | Dim::Hour | Dim::Weekday)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub account: Option<String>,
    pub seat: Option<String>,
    /// Matches the project name or key.
    pub project: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub key: String,
    pub label: String,
    pub requests: u64,
    pub usage: Usage,
    /// USD of the priced requests.
    pub cost: f64,
    /// Requests of models without a known price (not in `cost`).
    pub unpriced: u64,
    /// Requests not priced yet: just read, or being repriced by another process (not in `cost`).
    pub pending: u64,
}

/// `FROM requests r` with only the joins the other parts of the query refer to: SQLite does not
/// drop unused LEFT JOINs, and each one costs a lookup per row.
fn from_for(parts: &[&str]) -> String {
    let uses = |alias: char| {
        parts.iter().any(|p| {
            let b = p.as_bytes();
            (0..b.len().saturating_sub(1)).any(|i| {
                b[i] == alias as u8
                    && b[i + 1] == b'.'
                    && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
            })
        })
    };
    let mut s = String::from("FROM requests r");
    if uses('p') {
        s.push_str(" LEFT JOIN projects p ON p.cwd = r.cwd");
    }
    if uses('a') {
        s.push_str(" LEFT JOIN agents a ON a.seat = r.seat AND a.session_id = r.session_id AND a.agent_id = r.agent_id");
    }
    if uses('s') {
        s.push_str(" LEFT JOIN sessions s ON s.seat = r.seat AND s.session_id = r.session_id");
    }
    if uses('c') {
        s.push_str(" LEFT JOIN accounts c ON c.account_uuid = r.account_uuid");
    }
    s
}

fn where_clause(span: Span, f: &Filter, args: &mut Vec<rusqlite::types::Value>) -> String {
    use rusqlite::types::Value as V;
    let mut w = vec![
        "r.ts >= ?".to_string(),
        "r.ts < ?".to_string(),
        "r.model != '<synthetic>'".to_string(),
    ];
    args.push(V::Integer(ms(span
        .start
        .max(DateTime::<Utc>::from_timestamp(0, 0).unwrap()))));
    args.push(V::Integer(ms(span.end)));
    if let Some(a) = &f.account {
        // A uuid uses the (account_uuid, ts) index; an email is looked up once.
        if a.contains('@') {
            w.push("r.account_uuid IN (SELECT account_uuid FROM accounts WHERE email = ?)".into());
        } else {
            w.push("r.account_uuid = ?".into());
        }
        args.push(V::Text(a.clone()));
    }
    if let Some(s) = &f.seat {
        w.push("r.seat = ?".into());
        args.push(V::Text(s.clone()));
    }
    if let Some(p) = &f.project {
        w.push("(p.name = ? OR p.key = ?)".into());
        args.push(V::Text(p.clone()));
        args.push(V::Text(p.clone()));
    }
    if let Some(m) = &f.model {
        w.push("r.model LIKE ?".into());
        args.push(V::Text(format!("%{m}%")));
    }
    format!("WHERE {}", w.join(" AND "))
}

/// Request count, token sums, then cost (NULL costs left out), unpriced and pending counts.
const SUMS: &str = "COUNT(*), SUM(r.input), SUM(r.cw5), SUM(r.cw1h), SUM(r.cread), SUM(r.output), SUM(r.thinking), SUM(r.web),
  TOTAL(r.cost), SUM(r.priced = 0), SUM(r.priced IS NULL)";

/// (requests, usage, cost, unpriced, pending)
type Sums = (u64, Usage, f64, u64, u64);

fn row_usage(r: &rusqlite::Row, at: usize) -> rusqlite::Result<Sums> {
    let g = |i: usize| -> rusqlite::Result<u64> {
        Ok(r.get::<_, Option<i64>>(at + i)?.unwrap_or(0) as u64)
    };
    Ok((
        g(0)?,
        Usage {
            input: g(1)?,
            cache_write_5m: g(2)?,
            cache_write_1h: g(3)?,
            cache_read: g(4)?,
            output: g(5)?,
            thinking: g(6)?,
            web_search: g(7)?,
        },
        r.get::<_, f64>(at + 8)?,
        g(9)?,
        g(10)?,
    ))
}

/// One UTC quarter hour (`period::BUCKET_MS`): (bucket number, requests, usage, cost, unpriced,
/// pending).
type BucketRow = (i64, u64, Usage, f64, u64, u64);

impl Index {
    pub fn spend_by(&self, dim: Dim, span: Span, f: &Filter) -> anyhow::Result<Vec<Row>> {
        if dim.is_time() {
            return self.spend_by_time(dim, span, f);
        }
        let (key, label) = dim.exprs();
        let mut args = Vec::new();
        let wh = where_clause(span, f, &mut args);
        let order = if dim.is_time() { "1 ASC" } else { "11 DESC" }; // 11: the cost
        let from = from_for(&[key, label, &wh]);
        let sql = format!("SELECT {key}, {label}, {SUMS} {from} {wh} GROUP BY 1 ORDER BY {order}");
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args), |r| {
            let (requests, usage, cost, unpriced, pending) = row_usage(r, 2)?;
            Ok(Row {
                key: r.get(0)?,
                label: r.get(1)?,
                requests,
                usage,
                cost,
                unpriced,
                pending,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Sums per UTC quarter hour; local-time grouping happens in Rust (`time_key`), which is far
    /// cheaper than SQLite's `localtime` on every row and exact in every zone.
    fn buckets(&self, span: Span, f: &Filter) -> anyhow::Result<Vec<BucketRow>> {
        let mut args = Vec::new();
        let wh = where_clause(span, f, &mut args);
        let from = from_for(&[&wh]);
        let sql = format!("SELECT r.ts / {BUCKET_MS}, {SUMS} {from} {wh} GROUP BY 1");
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args), |r| {
            let (n, u, c, up, pe) = row_usage(r, 1)?;
            Ok((r.get::<_, i64>(0)?, n, u, c, up, pe))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn time_key(dim: Dim, bucket: i64) -> String {
        time_key_in(dim, bucket, &chrono::Local)
    }
    fn spend_by_time(&self, dim: Dim, span: Span, f: &Filter) -> anyhow::Result<Vec<Row>> {
        let mut acc: BTreeMap<String, Row> = BTreeMap::new();
        for (h, n, u, c, up, pe) in self.buckets(span, f)? {
            let k = Self::time_key(dim, h);
            let e = acc.entry(k.clone()).or_insert_with(|| Row {
                key: k.clone(),
                label: k,
                requests: 0,
                usage: Usage::default(),
                cost: 0.0,
                unpriced: 0,
                pending: 0,
            });
            e.requests += n;
            e.usage.add(&u);
            e.cost += c;
            e.unpriced += up;
            e.pending += pe;
        }
        Ok(acc.into_values().collect())
    }

    /// Spend grouped by two dimensions (e.g. project × model).
    pub fn matrix(
        &self,
        a: Dim,
        b: Dim,
        span: Span,
        f: &Filter,
    ) -> anyhow::Result<Vec<(String, String, f64)>> {
        if a.is_time() && b.is_time() {
            let mut acc: BTreeMap<(String, String), f64> = BTreeMap::new();
            for (h, _, _, c, _, _) in self.buckets(span, f)? {
                *acc.entry((Self::time_key(a, h), Self::time_key(b, h)))
                    .or_default() += c;
            }
            return Ok(acc.into_iter().map(|((x, y), c)| (x, y, c)).collect());
        }
        let (ka, la) = a.exprs();
        let (kb, _) = b.exprs();
        let mut args = Vec::new();
        let wh = where_clause(span, f, &mut args);
        let la = if la.starts_with("MAX(") {
            la.to_string()
        } else {
            format!("MAX({la})")
        };
        let from = from_for(&[&la, kb, ka, &wh]);
        let sql = format!("SELECT {la}, {kb}, TOTAL(r.cost) {from} {wh} GROUP BY {ka}, {kb}");
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args), |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn total(&self, span: Span, f: &Filter) -> anyhow::Result<Row> {
        total_on(&self.conn, span, f)
    }

    /// `total` together with the price fingerprint its costs were computed with, read in one
    /// snapshot (so a reprice committed in between cannot pair new costs with the old prices).
    pub fn total_priced(&self, span: Span, f: &Filter) -> anyhow::Result<(Row, Option<String>)> {
        let tx = self.conn.unchecked_transaction()?;
        let fp = super::schema::meta(&tx, "price_fp")?;
        let row = total_on(&tx, span, f)?;
        tx.commit()?;
        Ok((row, fp))
    }

    /// The spend of an account over a span. A span that ended before the settled horizon
    /// ([`super::SETTLED_AFTER_MS`]) is summed once and remembered (the dashboard asks every
    /// minute), keyed by the settled generation and the price fingerprint read together with the
    /// total: a reprice, a re-attribution or a late request (here or in another process) is never
    /// cached away. A total with requests still waiting for a price is not remembered.
    pub fn settled_total(
        &self,
        account: &str,
        span: Span,
        now: DateTime<Utc>,
    ) -> anyhow::Result<f64> {
        let filter = Filter {
            account: Some(account.to_string()),
            ..Default::default()
        };
        if ms(span.end) >= ms(now) - super::SETTLED_AFTER_MS {
            return Ok(self.total(span, &filter)?.cost);
        }
        let tx = self.conn.unchecked_transaction()?;
        let generation = super::schema::meta(&tx, "settled_gen")?;
        let fp = super::schema::meta(&tx, "price_fp")?;
        let key = |fp: &str| {
            (
                generation.clone(),
                fp.to_string(),
                account.to_string(),
                ms(span.start),
                ms(span.end),
            )
        };
        if let Some(fp) = &fp
            && let Some(v) = self.settled.borrow().get(&key(fp))
        {
            return Ok(*v);
        }
        let row = total_on(&tx, span, &filter)?;
        tx.commit()?;
        if row.pending == 0
            && let Some(fp) = fp
        {
            let mut m = self.settled.borrow_mut();
            if m.len() > 4096 {
                m.clear();
            }
            m.insert(key(&fp), row.cost);
        }
        Ok(row.cost)
    }

    /// Requests whose model differs from the one asked for.
    pub fn model_substitutions(
        &self,
        span: Span,
        f: &Filter,
    ) -> anyhow::Result<Vec<(String, String, u64)>> {
        let mut args = Vec::new();
        let wh = where_clause(span, f, &mut args);
        let sql = format!(
            "SELECT r.requested_model, r.model, COUNT(*) {} {wh} AND r.requested_model IS NOT NULL AND r.requested_model != ''
             AND r.requested_model != r.model GROUP BY 1, 2 ORDER BY 3 DESC",
            from_for(&[&wh])
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every stored utilization reading of an account and window, oldest first.
    pub fn readings(&self, account: &str, kind: WindowKind) -> anyhow::Result<Vec<Reading>> {
        let mut st = self.conn.prepare("SELECT fetched_at, utilization, resets_at FROM readings WHERE account_uuid = ?1 AND kind = ?2 ORDER BY fetched_at")?;
        let rows = st.query_map(rusqlite::params![account, kind.key()], |r| {
            Ok(Reading {
                kind,
                fetched_at: from_ms(r.get(0)?),
                utilization: r.get(1)?,
                resets_at: from_ms(r.get(2)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The newest reading of every account and window kind.
    pub fn latest_readings(&self) -> anyhow::Result<Vec<(String, Vec<Reading>)>> {
        let mut st = self.conn.prepare(
            "SELECT r.account_uuid, r.kind, r.fetched_at, r.utilization, r.resets_at FROM readings r
             JOIN (SELECT account_uuid, kind, MAX(fetched_at) AS m FROM readings GROUP BY 1, 2) x
               ON x.account_uuid = r.account_uuid AND x.kind = r.kind AND x.m = r.fetched_at
             ORDER BY 1, 2",
        )?;
        let mut out: BTreeMap<String, Vec<Reading>> = BTreeMap::new();
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (acct, kind, f, u, z) = row?;
            if let Some(kind) = WindowKind::from_key(&kind) {
                out.entry(acct).or_default().push(Reading {
                    kind,
                    fetched_at: from_ms(f),
                    utilization: u,
                    resets_at: from_ms(z),
                });
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Claude Code's own totals vs. the transcript sums, over sessions that have both:
    /// (sessions, claude code USD, transcript USD).
    pub fn cost_crosscheck(&self, span: Span) -> anyhow::Result<(u64, f64, f64)> {
        let sql = "SELECT COUNT(*), SUM(cs.total_usd), SUM(t.cost) FROM cost_states cs
                   JOIN (SELECT seat, session_id, TOTAL(cost) AS cost, MAX(ts) AS last FROM requests GROUP BY 1, 2) t
                     ON t.seat = cs.seat AND t.session_id = cs.session_id
                   WHERE t.last >= ?1 AND t.last < ?2";
        Ok(self.conn.query_row(
            sql,
            rusqlite::params![
                ms(span
                    .start
                    .max(DateTime::<Utc>::from_timestamp(0, 0).unwrap())),
                ms(span.end)
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                ))
            },
        )?)
    }

    pub fn count(&self, table: &str) -> anyhow::Result<u64> {
        Ok(self
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })? as u64)
    }

    pub fn record_types(&self) -> anyhow::Result<Vec<(String, u64)>> {
        let mut st = self
            .conn
            .prepare("SELECT type, SUM(count) FROM record_types GROUP BY 1 ORDER BY 2 DESC")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn api_errors(&self, span: Span) -> anyhow::Result<Vec<(String, u64)>> {
        let mut st = self.conn.prepare("SELECT kind, COUNT(*) FROM api_errors WHERE ts >= ?1 AND ts < ?2 GROUP BY 1 ORDER BY 2 DESC")?;
        let rows = st.query_map(
            rusqlite::params![
                ms(span
                    .start
                    .max(DateTime::<Utc>::from_timestamp(0, 0).unwrap())),
                ms(span.end)
            ],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Spend per bucket of `bucket_minutes` over a span, for sparklines and burn rate.
    pub fn spend_series(
        &self,
        span: Span,
        bucket_minutes: i64,
        f: &Filter,
    ) -> anyhow::Result<Vec<f64>> {
        let mut args = Vec::new();
        let wh = where_clause(span, f, &mut args);
        let bucket = bucket_minutes.max(1) * 60_000;
        let start = ms(span.start);
        let n = (((ms(span.end) - start) as f64) / bucket as f64)
            .ceil()
            .max(1.0) as usize;
        let sql = format!(
            "SELECT (r.ts - {start}) / {bucket}, TOTAL(r.cost) {} {wh} GROUP BY 1",
            from_for(&[&wh])
        );
        let mut st = self.conn.prepare(&sql)?;
        let mut out = vec![0.0; n];
        let rows = st.query_map(params_from_iter(args), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            ))
        })?;
        for row in rows {
            let (i, c) = row?;
            if (i as usize) < n && i >= 0 {
                out[i as usize] = c;
            }
        }
        Ok(out)
    }
}

/// The label of a time dimension for a bucket, in the zone `tz` (`Local` outside tests).
fn time_key_in<Tz: chrono::TimeZone>(dim: Dim, bucket: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let t = bucket_start(bucket, tz);
    match dim {
        Dim::Day => t.format("%Y-%m-%d").to_string(),
        Dim::Hour => t.format("%H").to_string(),
        _ => t.format("%w").to_string(),
    }
}

fn total_on(conn: &rusqlite::Connection, span: Span, f: &Filter) -> anyhow::Result<Row> {
    let mut args = Vec::new();
    let wh = where_clause(span, f, &mut args);
    let from = from_for(&[&wh]);
    let sql = format!("SELECT {SUMS} {from} {wh}");
    Ok(conn.query_row(&sql, params_from_iter(args), |r| {
        let (requests, usage, cost, unpriced, pending) = row_usage(r, 0)?;
        Ok(Row {
            key: "total".into(),
            label: "total".into(),
            requests,
            usage,
            cost,
            unpriced,
            pending,
        })
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn time_keys_follow_the_local_offset() {
        // 00:10 local on Sunday 11-22 in +05:30 is 18:40 UTC on Saturday: day, hour and weekday
        // are the local ones.
        let tz = FixedOffset::east_opt(330 * 60).unwrap();
        let t = DateTime::parse_from_rfc3339("2026-11-22T00:10:00+05:30").unwrap();
        let b = t.timestamp_millis() / BUCKET_MS;
        assert_eq!(time_key_in(Dim::Day, b, &tz), "2026-11-22");
        assert_eq!(time_key_in(Dim::Hour, b, &tz), "00");
        assert_eq!(time_key_in(Dim::Weekday, b, &tz), "0");
        // 23:55 local on 11-21 in −03:30 is 03:25 UTC on 11-22.
        let tz = FixedOffset::west_opt(210 * 60).unwrap();
        let t = DateTime::parse_from_rfc3339("2026-11-21T23:55:00-03:30").unwrap();
        let b = t.timestamp_millis() / BUCKET_MS;
        assert_eq!(time_key_in(Dim::Day, b, &tz), "2026-11-21");
        assert_eq!(time_key_in(Dim::Hour, b, &tz), "23");
    }
}
