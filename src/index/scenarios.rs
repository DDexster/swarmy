//! Scenario tests of the index: synthetic seats and transcripts in a temp dir, driven through the
//! index's public API (`open`, `refresh`, `price_all`, `dated_prices`, the queries). Money is
//! computed by hand in the comments. `World` is the fixture, shared with the limits tests.

#[cfg(test)]
pub(crate) use tests::{World, account, assistant, at, owner, weekly};

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::PathBuf;

    use chrono::{DateTime, TimeZone, Utc};

    use crate::claude::config_json::{Account, Utilization};
    use crate::claude::project::{Alias, Resolver};
    use crate::claude::seats::Seat;
    use crate::core::limits::{Reading, WindowKind};
    use crate::core::period::Span;
    use crate::core::pricing::{PriceBook, PriceEntry, PriceSource, RateOverrides, Rates};
    use crate::index::query::{Dim, Filter};
    use crate::index::{Index, SeatInfo};

    /// Seats and transcripts under a temp dir. Never a real config dir.
    pub(crate) struct World {
        pub dir: tempfile::TempDir,
        pub seats: Vec<Seat>,
    }

    pub(crate) fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// One assistant record (one content block of a response): `input` fresh input tokens,
    /// `output` output tokens, and the given tool uses.
    pub(crate) fn assistant(
        msg_id: &str,
        ts: &str,
        model: &str,
        cwd: &str,
        (input, output): (u64, u64),
        tools: &[&str],
    ) -> String {
        let content: Vec<serde_json::Value> = tools
            .iter()
            .map(|id| serde_json::json!({"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "ls"}}))
            .collect();
        serde_json::json!({
            "type": "assistant", "timestamp": ts, "cwd": cwd, "sessionId": "s",
            "message": {"id": msg_id, "model": model, "content": content,
                "usage": {"input_tokens": input, "output_tokens": output,
                          "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}
        })
        .to_string()
    }

    pub(crate) fn owner(account: &str) -> String {
        serde_json::json!({"type": "session-owner", "accountUuid": account, "sessionId": "s"})
            .to_string()
    }

    pub(crate) fn compact(ts: &str) -> String {
        serde_json::json!({"type": "system", "subtype": "compact_boundary", "timestamp": ts})
            .to_string()
    }

    pub(crate) fn account(uuid: &str, since: &str) -> Account {
        Account {
            account_uuid: uuid.into(),
            profile_fetched_at: Some(at(since)),
            ..Default::default()
        }
    }

    pub(crate) fn weekly(account: &str, fetched: &str, used: f64, resets: &str) -> Utilization {
        Utilization {
            account_uuid: account.into(),
            fetched_at: at(fetched),
            readings: vec![Reading {
                kind: WindowKind::SevenDay,
                fetched_at: at(fetched),
                utilization: used,
                resets_at: at(resets),
            }],
            extra_usage_enabled: false,
        }
    }

    impl World {
        pub(crate) fn new(labels: &[&str]) -> World {
            let dir = tempfile::tempdir().unwrap();
            let seats = labels
                .iter()
                .map(|l| {
                    let d = dir.path().join(format!("claude-{l}"));
                    std::fs::create_dir_all(d.join("projects").join("-w-proj")).unwrap();
                    Seat {
                        label: l.to_string(),
                        claude_json: d.join(".claude.json"),
                        dir: d,
                        is_default: false,
                        found_by: Vec::new(),
                    }
                })
                .collect();
            World { dir, seats }
        }

        fn transcript(&self, seat: &str, session: &str) -> PathBuf {
            let s = self.seats.iter().find(|s| s.label == seat).unwrap();
            s.projects_dir()
                .join("-w-proj")
                .join(format!("{session}.jsonl"))
        }

        pub(crate) fn append(&self, seat: &str, session: &str, lines: &[String]) {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.transcript(seat, session))
                .unwrap();
            for l in lines {
                writeln!(f, "{l}").unwrap();
            }
        }

        /// Writes the transcript anew (another inode), as when a file is replaced.
        pub(crate) fn replace(&self, seat: &str, session: &str, lines: &[String]) {
            let p = self.transcript(seat, session);
            let tmp = p.with_extension("tmp");
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&tmp, text).unwrap();
            std::fs::rename(&tmp, &p).unwrap();
        }

        pub(crate) fn index(&self, name: &str) -> Index {
            Index::open(&self.dir.path().join(name)).unwrap()
        }

        pub(crate) fn refresh(&self, idx: &mut Index, info: &[SeatInfo], prices: &PriceBook) {
            let mut resolver = Resolver::new(vec![
                Alias {
                    prefix: "/w/shop".into(),
                    name: "shop".into(),
                },
                Alias {
                    prefix: "/w/docs".into(),
                    name: "docs".into(),
                },
            ]);
            idx.refresh(&self.seats, info, &mut resolver, prices, &mut |_| {})
                .unwrap();
        }
    }

    /// Every row of a query, as text, for comparing two indexes.
    fn dump(idx: &Index, sql: &str) -> Vec<String> {
        let mut st = idx.conn.prepare(sql).unwrap();
        let n = st.column_count();
        st.query_map([], |r| {
            use rusqlite::types::ValueRef as V;
            Ok((0..n)
                .map(|i| match r.get_ref(i).unwrap() {
                    V::Null => "NULL".to_string(),
                    V::Integer(x) => x.to_string(),
                    V::Real(x) => format!("{x:.9}"),
                    V::Text(t) | V::Blob(t) => String::from_utf8_lossy(t).into_owned(),
                })
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    const REQUESTS: &str = "SELECT msg_id, seat, session_id, agent_id, account_uuid, ts, model, input, output, tools, cost, priced FROM requests ORDER BY msg_id";
    const SESSIONS: &str =
        "SELECT seat, session_id, first_ts, last_ts, compactions FROM sessions ORDER BY 1, 2";
    const TOOLS: &str = "SELECT id, msg_id FROM tool_calls ORDER BY id";

    fn all(span_end: &str) -> Span {
        Span {
            start: Utc.timestamp_opt(0, 0).unwrap(),
            end: at(span_end),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// A session in three parts. msg_2 is one response written as two records (two tool uses,
    /// one per record) that straddle the first cut; a compaction sits in the last part.
    fn session_parts() -> [Vec<String>; 3] {
        let m = "claude-sonnet-5-5";
        [
            vec![
                owner("acc-a"),
                assistant(
                    "msg_1",
                    "2026-11-10T10:00:00Z",
                    m,
                    "/w/shop",
                    (1000, 10),
                    &["t1"],
                ),
                assistant(
                    "msg_2",
                    "2026-11-10T10:01:00Z",
                    m,
                    "/w/shop",
                    (2000, 5),
                    &["t2"],
                ),
            ],
            vec![
                assistant(
                    "msg_2",
                    "2026-11-10T10:01:00Z",
                    m,
                    "/w/shop",
                    (2000, 50),
                    &["t3"],
                ),
                compact("2026-11-10T10:02:00Z"),
            ],
            vec![assistant(
                "msg_3",
                "2026-11-10T10:03:00Z",
                m,
                "/w/docs",
                (500, 100),
                &[],
            )],
        ]
    }

    #[test]
    fn incremental_refresh_equals_a_full_rebuild_and_a_reread_counts_once() {
        let w = World::new(&["work"]);
        let prices = PriceBook::builtin();
        let mut inc = w.index("inc.sqlite");
        let parts = session_parts();
        for p in &parts {
            w.append("work", "s1", p);
            w.refresh(&mut inc, &[], &prices);
        }
        let mut full = w.index("full.sqlite");
        w.refresh(&mut full, &[], &prices);
        for q in [REQUESTS, SESSIONS, TOOLS] {
            assert_eq!(dump(&inc, q), dump(&full, q), "{q}");
        }
        // The file is replaced with the same content (a new inode): read again from the start,
        // tools and compactions still count once.
        w.replace("work", "s1", &parts.concat());
        w.refresh(&mut inc, &[], &prices);
        for q in [REQUESTS, SESSIONS, TOOLS] {
            assert_eq!(dump(&inc, q), dump(&full, q), "after the re-read: {q}");
        }
        let tools: i64 = inc
            .conn
            .query_row("SELECT SUM(tools) FROM requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tools, 3, "t1, t2, t3");
        let compactions: i64 = inc
            .conn
            .query_row("SELECT compactions FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(compactions, 1);
    }

    #[test]
    fn records_of_one_response_merge_by_message_id() {
        let w = World::new(&["work"]);
        let mut idx = w.index("i.sqlite");
        w.append("work", "s1", &session_parts().concat());
        w.refresh(&mut idx, &[], &PriceBook::builtin());
        let t = idx
            .total(all("2026-11-11T00:00:00Z"), &Filter::default())
            .unwrap();
        // msg_2's two records count once, with the larger output (50, not 5 + 50).
        assert_eq!(t.requests, 3);
        assert_eq!(t.usage.input, 1000 + 2000 + 500);
        assert_eq!(t.usage.output, 10 + 50 + 100);
        // Sonnet 5.5 at $2 in / $10 out per MTok: 3500 × 2 + 160 × 10 = 7000 + 1600 = 8600 µ$.
        assert!(close(t.cost, 0.0086), "{}", t.cost);
        assert_eq!((t.unpriced, t.pending), (0, 0));
        let by_project = idx
            .spend_by(
                Dim::Project,
                all("2026-11-11T00:00:00Z"),
                &Filter::default(),
            )
            .unwrap();
        // shop: msg_1 + msg_2 = 3000 × 2 + 60 × 10 = 6600 µ$; docs: 500 × 2 + 100 × 10 = 2000 µ$.
        assert_eq!(by_project[0].label, "shop");
        assert!(close(by_project[0].cost, 0.0066));
        assert_eq!(by_project[1].label, "docs");
        assert!(close(by_project[1].cost, 0.002));
    }

    #[test]
    fn a_login_swap_moves_later_requests_and_survives_a_cache_rebuild() {
        let w = World::new(&["work"]);
        let m = "claude-sonnet-5-5";
        // The session started under acc-a and keeps writing acc-a as its owner; the seat's login
        // was swapped to acc-b at 11:00.
        w.append(
            "work",
            "s1",
            &[
                owner("acc-a"),
                assistant("r1", "2026-11-10T10:00:00Z", m, "/w/shop", (1000, 0), &[]),
                assistant("r2", "2026-11-10T12:00:00Z", m, "/w/shop", (1000, 0), &[]),
            ],
        );
        let info = [SeatInfo {
            label: "work".into(),
            account: Some(account("acc-b", "2026-11-10T11:00:00Z")),
            utilization: None,
        }];
        let path = w.dir.path().join("i.sqlite");
        let mut idx = Index::open(&path).unwrap();
        w.refresh(&mut idx, &info, &PriceBook::builtin());
        let accounts = |idx: &Index| {
            dump(
                idx,
                "SELECT msg_id, account_uuid FROM requests ORDER BY msg_id",
            )
        };
        let expected = vec!["r1|acc-a".to_string(), "r2|acc-b".to_string()];
        assert_eq!(accounts(&idx), expected);
        // A request written after the swap follows the seat at once.
        w.append(
            "work",
            "s1",
            &[assistant(
                "r3",
                "2026-11-10T13:00:00Z",
                m,
                "/w/shop",
                (1000, 0),
                &[],
            )],
        );
        w.refresh(&mut idx, &info, &PriceBook::builtin());
        assert_eq!(accounts(&idx).last().unwrap(), "r3|acc-b");

        // A new cache version: the cache is rebuilt from the transcripts. The seat no longer
        // reports any account (logged out), so only the stored login history can tell.
        drop(idx);
        Index::open(&path)
            .unwrap()
            .set_meta("schema_version", "0")
            .unwrap();
        let mut idx = Index::open(&path).unwrap();
        assert_eq!(idx.count("requests").unwrap(), 0, "the cache was dropped");
        assert_eq!(
            idx.count("seat_accounts").unwrap(),
            1,
            "the facts were kept"
        );
        w.refresh(&mut idx, &[], &PriceBook::builtin());
        let after = accounts(&idx);
        assert_eq!(after[..2], expected[..]);
        assert_eq!(after[2], "r3|acc-b");
    }

    fn cache_book(input: f64, output: f64) -> PriceBook {
        let mut b = PriceBook::builtin();
        let cache = serde_json::json!({"claude-sonnet-5-5": {"inputTokens": input, "outputTokens": output}});
        b.merge_claude_code_cache("work", &cache);
        b
    }

    #[test]
    fn a_price_change_reprices_the_index_but_not_the_past() {
        let w = World::new(&["work"]);
        let m = "claude-sonnet-5-5";
        // One million input tokens each, at noon UTC, days apart (any time zone keeps the day).
        w.append(
            "work",
            "s1",
            &[
                assistant(
                    "a",
                    "2026-11-02T12:00:00Z",
                    m,
                    "/w/shop",
                    (1_000_000, 0),
                    &[],
                ),
                assistant(
                    "b",
                    "2026-11-08T12:00:00Z",
                    m,
                    "/w/shop",
                    (1_000_000, 0),
                    &[],
                ),
                assistant(
                    "c",
                    "2026-11-14T12:00:00Z",
                    m,
                    "/w/shop",
                    (1_000_000, 0),
                    &[],
                ),
                assistant(
                    "u",
                    "2026-11-14T12:00:00Z",
                    "claude-unknown-9",
                    "/w/shop",
                    (10, 0),
                    &[],
                ),
            ],
        );
        let mut idx = w.index("i.sqlite");
        let cost = |idx: &Index, day: &str| {
            idx.total(
                Span {
                    start: at(&format!("{day}T00:00:00Z")),
                    end: at(&format!("{day}T23:59:59Z")),
                },
                &Filter {
                    model: Some("sonnet".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .cost
        };
        // Built-in prices: $2 per MTok of input, so $2 each.
        w.refresh(&mut idx, &[], &PriceBook::builtin());
        for d in ["2026-11-02", "2026-11-08", "2026-11-14"] {
            assert!(close(cost(&idx, d), 2.0), "{d}");
        }
        // The unknown model is counted as unpriced, never as $0 inside the cost.
        let t = idx
            .total(all("2026-12-01T00:00:00Z"), &Filter::default())
            .unwrap();
        assert_eq!((t.requests, t.unpriced, t.pending), (4, 1, 0));
        assert!(close(t.cost, 6.0));

        // Claude Code's cache shows $3 on 11-05, then $4 on 11-11.
        let v1 = cache_book(3.0, 15.0);
        idx.dated_prices(&v1, at("2026-11-05T12:00:00Z")).unwrap();
        idx.price_all(&v1).unwrap();
        // Before the first sighting the built-in price stays; from 11-05 the cache's.
        assert!(close(cost(&idx, "2026-11-02"), 2.0));
        assert!(close(cost(&idx, "2026-11-08"), 3.0));
        assert!(close(cost(&idx, "2026-11-14"), 3.0));
        let v2 = cache_book(4.0, 20.0);
        idx.dated_prices(&v2, at("2026-11-11T12:00:00Z")).unwrap();
        idx.price_all(&v2).unwrap();
        assert!(close(cost(&idx, "2026-11-02"), 2.0));
        assert!(
            close(cost(&idx, "2026-11-08"), 3.0),
            "the past keeps its price"
        );
        assert!(close(cost(&idx, "2026-11-14"), 4.0));
        // The fingerprint the costs were computed with is the one read with them.
        let (row, fp) = idx
            .total_priced(all("2026-12-01T00:00:00Z"), &Filter::default())
            .unwrap();
        assert_eq!(fp, idx.meta("price_fp").unwrap());
        assert!(close(row.cost, 2.0 + 3.0 + 4.0));

        // Another process (or a restart) dates the same cache the same way: same costs, no
        // reprice back and forth.
        let fp_before = idx.meta("price_fp").unwrap();
        drop(idx);
        let mut again = w.index("i.sqlite");
        again.price_all(&v2).unwrap();
        assert_eq!(again.meta("price_fp").unwrap(), fp_before);
        assert!(close(cost(&again, "2026-11-08"), 3.0));

        // A user override reprices everything it covers, keeping fast mode and cache ratios.
        let mut user = cache_book(4.0, 20.0);
        user.push_override(
            m,
            None,
            1.0,
            5.0,
            &RateOverrides::default(),
            at("2026-11-22T12:00:00Z").date_naive(),
        )
        .unwrap();
        again.price_all(&user).unwrap();
        for d in ["2026-11-02", "2026-11-08", "2026-11-14"] {
            assert!(close(cost(&again, d), 1.0), "{d}");
        }
    }

    #[test]
    fn the_first_sighting_of_an_unknown_model_prices_its_whole_history() {
        // A model newer than the built-in table: only Claude Code's cache prices it, so its first
        // version applies before the day it was seen too.
        let w = World::new(&["work"]);
        w.append(
            "work",
            "s1",
            &[assistant(
                "n",
                "2026-10-01T12:00:00Z",
                "claude-new-9",
                "/w/shop",
                (1_000_000, 0),
                &[],
            )],
        );
        let mut idx = w.index("i.sqlite");
        let mut book = PriceBook::builtin();
        book.merge_claude_code_cache(
            "work",
            &serde_json::json!({"claude-new-9": {"inputTokens": 7, "outputTokens": 35}}),
        );
        let dated = idx.dated_prices(&book, at("2026-11-20T12:00:00Z")).unwrap();
        let e: &PriceEntry = dated
            .entries()
            .iter()
            .find(|e| e.model == "claude-new-9")
            .unwrap();
        assert_eq!(e.from, None);
        assert_eq!(e.source, PriceSource::ClaudeCode("work".into()));
        let expected = Rates::derive(7.0, 35.0, &RateOverrides::default(), None);
        assert_eq!(e.rates, expected);
        w.refresh(&mut idx, &[], &book);
        let t = idx
            .total(all("2026-12-01T00:00:00Z"), &Filter::default())
            .unwrap();
        // 1M input at $7.
        assert!(close(t.cost, 7.0), "{}", t.cost);
        assert_eq!(t.unpriced, 0);
    }
}
