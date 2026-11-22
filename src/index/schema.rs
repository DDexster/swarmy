//! The index schema, in two parts.
//!
//! **Cache tables** hold what the transcripts say. When `CACHE_VERSION` changes they are dropped
//! and rebuilt from the transcripts.
//!
//! **Fact tables** hold what swarmy observed over time and nothing else records: which account
//! each seat had logged in and since when (`seat_accounts`), the accounts seen (`accounts`), limit
//! readings (`readings`), and the dates Claude Code's prices were first seen (`observed_prices`).
//! They are never dropped; `FACTS_VERSION` steps migrate them.
//!
//! The rules of the data are constraints in the schema, so every write path is checked, and the
//! lists of enum-like values are generated from the Rust enums (`WindowKind::ALL`,
//! `FileKind::TAGS`) so the two cannot drift.

use rusqlite::{Connection, OptionalExtension, params};

use crate::claude::files::FileKind;
use crate::core::limits::WindowKind;

/// Version of the cache tables; a change drops and rebuilds them (never the facts).
pub const CACHE_VERSION: &str = "6";

/// Version of the fact tables; each step up has a migration in `migrate_facts`.
pub const FACTS_VERSION: u32 = 1;

/// Derived from the transcripts: dropped and rebuilt when `CACHE_VERSION` changes.
pub const CACHE_TABLES: &[&str] = &[
    "files",
    "requests",
    "tool_calls",
    "sessions",
    "agents",
    "owners",
    "projects",
    "prompts",
    "api_errors",
    "cost_states",
    "record_types",
];

/// Observed over time and written nowhere else: migrated, never dropped.
pub const FACT_TABLES: &[&str] = &[
    "meta",
    "seat_accounts",
    "accounts",
    "readings",
    "observed_prices",
];

/// Keys of `meta` that describe the cache tables and go with them.
pub const CACHE_META: &[&str] = &["price_fp", "projects_rev"];

/// `'a', 'b', 'c'` for an `IN (...)` check.
fn in_list(keys: &[&str]) -> String {
    keys.iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn window_kinds() -> String {
    in_list(&WindowKind::ALL.map(WindowKind::key))
}

pub fn file_kinds() -> String {
    in_list(&FileKind::TAGS)
}

pub fn cache_sql() -> String {
    let file_kinds = file_kinds();
    format!(
        r#"
CREATE TABLE IF NOT EXISTS files (
  path TEXT PRIMARY KEY,
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ({file_kinds})),
  ino INTEGER NOT NULL DEFAULT 0 CHECK (ino >= 0),
  size INTEGER NOT NULL DEFAULT 0 CHECK (size >= 0),
  mtime INTEGER NOT NULL DEFAULT 0,
  offset INTEGER NOT NULL DEFAULT 0 CHECK (offset >= 0),
  last_ts INTEGER NOT NULL DEFAULT 0,
  account_uuid TEXT
);

CREATE TABLE IF NOT EXISTS requests (
  msg_id TEXT PRIMARY KEY,
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  account_uuid TEXT,
  ts INTEGER NOT NULL,
  model TEXT NOT NULL,
  requested_model TEXT,
  effort TEXT,
  fast INTEGER NOT NULL DEFAULT 0 CHECK (fast IN (0, 1)),
  cwd TEXT,
  input INTEGER NOT NULL DEFAULT 0 CHECK (input >= 0),
  cw5 INTEGER NOT NULL DEFAULT 0 CHECK (cw5 >= 0),
  cw1h INTEGER NOT NULL DEFAULT 0 CHECK (cw1h >= 0),
  cread INTEGER NOT NULL DEFAULT 0 CHECK (cread >= 0),
  output INTEGER NOT NULL DEFAULT 0 CHECK (output >= 0),
  thinking INTEGER NOT NULL DEFAULT 0 CHECK (thinking >= 0),
  web INTEGER NOT NULL DEFAULT 0 CHECK (web >= 0),
  tools INTEGER NOT NULL DEFAULT 0 CHECK (tools >= 0),
  -- USD at the prices of `meta.price_fp`; NULL while not priced, or when no price is known.
  cost REAL CHECK (cost IS NULL OR cost >= 0),
  -- NULL: not priced yet; 0: no price is known for the model; 1: priced.
  priced INTEGER CHECK (priced IN (0, 1)),
  CHECK (CASE WHEN priced = 1 THEN cost IS NOT NULL ELSE cost IS NULL END)
);
CREATE INDEX IF NOT EXISTS requests_ts ON requests(ts);
CREATE INDEX IF NOT EXISTS requests_agent ON requests(seat, session_id, agent_id, ts);
CREATE INDEX IF NOT EXISTS requests_pending ON requests(msg_id) WHERE priced IS NULL;

CREATE TABLE IF NOT EXISTS tool_calls (
  id TEXT PRIMARY KEY,
  msg_id TEXT,
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  ts INTEGER NOT NULL,
  name TEXT NOT NULL,
  summary TEXT,
  target TEXT,
  done_ts INTEGER,
  is_error INTEGER NOT NULL DEFAULT 0 CHECK (is_error IN (0, 1))
);
CREATE INDEX IF NOT EXISTS tool_calls_ts ON tool_calls(ts);
CREATE INDEX IF NOT EXISTS tool_calls_agent ON tool_calls(seat, session_id, agent_id, ts);

CREATE TABLE IF NOT EXISTS sessions (
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  title TEXT,
  ai_title TEXT,
  cwd TEXT,
  first_ts INTEGER,
  last_ts INTEGER,
  compactions INTEGER NOT NULL DEFAULT 0 CHECK (compactions >= 0),
  PRIMARY KEY (seat, session_id)
);

CREATE TABLE IF NOT EXISTS agents (
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ({file_kinds})),
  workflow_run TEXT,
  agent_type TEXT,
  description TEXT,
  tool_use_id TEXT,
  parent_agent_id TEXT,
  depth INTEGER CHECK (depth IS NULL OR depth >= 0),
  requested_model TEXT,
  effort TEXT,
  finished INTEGER NOT NULL DEFAULT 0 CHECK (finished IN (0, 1)),
  first_ts INTEGER,
  last_ts INTEGER,
  PRIMARY KEY (seat, session_id, agent_id)
);

-- The account of a session over time, from its `session-owner` records.
CREATE TABLE IF NOT EXISTS owners (
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  ts INTEGER NOT NULL,
  account_uuid TEXT NOT NULL,
  PRIMARY KEY (seat, session_id, ts, account_uuid)
);

CREATE TABLE IF NOT EXISTS projects (
  cwd TEXT PRIMARY KEY,
  key TEXT NOT NULL,
  name TEXT NOT NULL,
  worktree TEXT
);

CREATE TABLE IF NOT EXISTS prompts (
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  ts INTEGER NOT NULL,
  chars INTEGER NOT NULL CHECK (chars >= 0)
);
CREATE INDEX IF NOT EXISTS prompts_ts ON prompts(ts);

CREATE TABLE IF NOT EXISTS api_errors (
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  ts INTEGER NOT NULL,
  kind TEXT NOT NULL,
  text TEXT
);
CREATE INDEX IF NOT EXISTS api_errors_ts ON api_errors(ts);

CREATE TABLE IF NOT EXISTS cost_states (
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  total_usd REAL NOT NULL CHECK (total_usd >= 0),
  models TEXT NOT NULL,
  PRIMARY KEY (seat, session_id)
);

CREATE TABLE IF NOT EXISTS record_types (
  type TEXT PRIMARY KEY,
  count INTEGER NOT NULL CHECK (count >= 0)
);
"#
    )
}

pub fn facts_sql() -> String {
    let window_kinds = window_kinds();
    format!(
        r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- Which account each seat (config dir) had logged in, and since when: from the seat's own
-- `.claude.json` (profile time, usage readings). Sessions keep writing their old owner after a
-- login is swapped under them, so for a seat with these rows the seat decides.
CREATE TABLE IF NOT EXISTS seat_accounts (
  seat TEXT NOT NULL,
  ts INTEGER NOT NULL,
  account_uuid TEXT NOT NULL,
  PRIMARY KEY (seat, ts)
);

CREATE TABLE IF NOT EXISTS accounts (
  account_uuid TEXT PRIMARY KEY,
  email TEXT,
  display_name TEXT,
  org_name TEXT,
  plan TEXT,
  tier TEXT,
  seat TEXT,
  seen_at INTEGER
);

CREATE TABLE IF NOT EXISTS readings (
  account_uuid TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ({window_kinds})),
  fetched_at INTEGER NOT NULL,
  utilization REAL NOT NULL CHECK (utilization >= 0),
  resets_at INTEGER NOT NULL,
  PRIMARY KEY (account_uuid, kind, fetched_at)
);

-- Each version of a model's price in Claude Code's cache, from the day it was first seen; a
-- request is priced with the version in force on its day.
CREATE TABLE IF NOT EXISTS observed_prices (
  model TEXT NOT NULL,
  seen_at INTEGER NOT NULL,
  seen_on TEXT NOT NULL CHECK (date(seen_on) IS seen_on),
  seat TEXT NOT NULL,
  rates TEXT NOT NULL CHECK (json_valid(rates)),
  PRIMARY KEY (model, seen_at)
);
"#
    )
}

/// Indexes added after a cache version shipped; created on every open, cheaply.
pub const EXTRA_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS requests_account_ts ON requests(account_uuid, ts);
CREATE INDEX IF NOT EXISTS requests_no_account ON requests(seat, session_id) WHERE account_uuid IS NULL;
CREATE INDEX IF NOT EXISTS files_mtime ON files(mtime);
"#;

pub(super) fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

pub(super) fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
        .exists([name])
}

/// Brings a database to the current schema in one transaction: migrates the facts, rebuilds the
/// cache when its version changed. Returns whether the cache was rebuilt.
pub fn migrate(conn: &Connection) -> anyhow::Result<bool> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    let tx = conn.unchecked_transaction()?;
    migrate_facts(&tx)?;
    let rebuild = meta(&tx, "schema_version")?.as_deref() != Some(CACHE_VERSION);
    if rebuild {
        // Facts are never dropped, whatever the lists say.
        debug_assert!(CACHE_TABLES.iter().all(|t| !FACT_TABLES.contains(t)));
        for t in CACHE_TABLES {
            tx.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?;
        }
        for k in CACHE_META {
            tx.execute("DELETE FROM meta WHERE key = ?1", [k])?;
        }
        set_meta(&tx, "schema_version", CACHE_VERSION)?;
    }
    // Tables added without a version bump (all `IF NOT EXISTS`), then extra indexes.
    tx.execute_batch(&cache_sql())?;
    tx.execute_batch(EXTRA_INDEXES)?;
    tx.commit()?;
    Ok(rebuild)
}

fn migrate_facts(conn: &Connection) -> anyhow::Result<()> {
    let version: u32 = match meta(conn, "facts_version")? {
        Some(v) => v.parse()?,
        None => 0,
    };
    if version > FACTS_VERSION {
        anyhow::bail!(
            "the index was written by a newer swarmy (facts version {version}); update swarmy"
        );
    }
    // 0 → 1: the fact tables of cache versions ≤ 5 had no constraints. Copy them into the new
    // tables, leaving out rows the new rules refuse.
    let legacy = ["seat_accounts", "accounts", "readings"];
    let mut renamed = Vec::new();
    if version < 1 {
        for t in legacy {
            if table_exists(conn, t)? {
                conn.execute_batch(&format!("ALTER TABLE {t} RENAME TO {t}_v0;"))?;
                renamed.push(t);
            }
        }
    }
    conn.execute_batch(&facts_sql())?;
    for t in renamed {
        let copy = match t {
            "seat_accounts" => "INSERT INTO seat_accounts(seat, ts, account_uuid)
                 SELECT seat, ts, account_uuid FROM seat_accounts_v0"
                .to_string(),
            "accounts" => "INSERT INTO accounts(account_uuid, email, display_name, org_name, plan, tier, seat, seen_at)
                 SELECT account_uuid, email, display_name, org_name, plan, tier, seat, seen_at FROM accounts_v0"
                .to_string(),
            "readings" => format!(
                "INSERT INTO readings(account_uuid, kind, fetched_at, utilization, resets_at)
                 SELECT account_uuid, kind, fetched_at, utilization, resets_at FROM readings_v0
                 WHERE kind IN ({}) AND utilization >= 0",
                window_kinds()
            ),
            other => anyhow::bail!("no migration for table {other}"),
        };
        conn.execute_batch(&copy)?;
        conn.execute_batch(&format!("DROP TABLE {t}_v0;"))?;
    }
    set_meta(conn, "facts_version", &FACTS_VERSION.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table names in `CREATE TABLE` statements.
    fn created(sql: &str) -> Vec<String> {
        sql.split("CREATE TABLE IF NOT EXISTS ")
            .skip(1)
            .map(|s| s.split([' ', '(']).next().unwrap().to_string())
            .collect()
    }

    #[test]
    fn every_table_is_either_cache_or_fact() {
        let cache = created(&cache_sql());
        let facts = created(&facts_sql());
        assert_eq!(cache, CACHE_TABLES, "cache tables and CACHE_TABLES differ");
        assert_eq!(facts, FACT_TABLES, "fact tables and FACT_TABLES differ");
        assert!(cache.iter().all(|t| !facts.contains(t)));
    }

    #[test]
    fn check_lists_come_from_the_enums() {
        // A new variant must be added to `ALL` too (this match stops compiling otherwise).
        let n = |k: WindowKind| match k {
            WindowKind::FiveHour
            | WindowKind::SevenDay
            | WindowKind::SevenDayOpus
            | WindowKind::SevenDaySonnet => 4,
        };
        assert_eq!(WindowKind::ALL.len(), n(WindowKind::FiveHour));
        for k in WindowKind::ALL {
            assert_eq!(WindowKind::from_key(k.key()), Some(k));
            assert!(window_kinds().contains(&format!("'{}'", k.key())));
        }
        assert_eq!(
            window_kinds(),
            "'five_hour', 'seven_day', 'seven_day_opus', 'seven_day_sonnet'"
        );
        for k in [
            FileKind::Main,
            FileKind::Subagent,
            FileKind::Workflow("w".into()),
        ] {
            assert!(file_kinds().contains(&format!("'{}'", k.tag())));
        }
    }

    #[test]
    fn the_schema_refuses_what_the_rules_forbid() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        let bad = [
            "INSERT INTO readings VALUES ('a', 'monthly', 1, 10, 2)",
            "INSERT INTO readings VALUES ('a', 'seven_day', 1, -1, 2)",
            "INSERT INTO requests(msg_id, seat, session_id, agent_id, ts, model, input) VALUES ('m', 's', 'x', '', 1, 'm', -5)",
            "INSERT INTO requests(msg_id, seat, session_id, agent_id, ts, model, fast) VALUES ('m', 's', 'x', '', 1, 'm', 2)",
            "INSERT INTO requests(msg_id, seat, session_id, agent_id, ts, model, cost, priced) VALUES ('m', 's', 'x', '', 1, 'm', -1, 1)",
            "INSERT INTO requests(msg_id, seat, session_id, agent_id, ts, model, cost, priced) VALUES ('m', 's', 'x', '', 1, 'm', NULL, 1)",
            "INSERT INTO requests(msg_id, seat, session_id, agent_id, ts, model, cost) VALUES ('m', 's', 'x', '', 1, 'm', 0.5)",
            "INSERT INTO files(path, seat, session_id, agent_id, kind) VALUES ('p', 's', 'x', '', 'other')",
            "INSERT INTO agents(seat, session_id, agent_id, kind, finished) VALUES ('s', 'x', 'a', 'main', 3)",
            "INSERT INTO observed_prices VALUES ('m', 1, 'yesterday', 's', '{}')",
        ];
        for sql in bad {
            assert!(c.execute(sql, []).is_err(), "accepted: {sql}");
        }
        let good = [
            "INSERT INTO readings VALUES ('a', 'seven_day', 1, 104.5, 2)",
            "INSERT INTO requests(msg_id, seat, session_id, agent_id, ts, model) VALUES ('m1', 's', 'x', '', 1, 'm')",
            "INSERT INTO requests(msg_id, seat, session_id, agent_id, ts, model, cost, priced) VALUES ('m2', 's', 'x', '', 1, 'm', 0.5, 1)",
            "INSERT INTO requests(msg_id, seat, session_id, agent_id, ts, model, priced) VALUES ('m3', 's', 'x', '', 1, 'm', 0)",
        ];
        for sql in good {
            c.execute(sql, []).unwrap();
        }
    }

    #[test]
    fn legacy_fact_tables_are_migrated_not_dropped() {
        // A version-5 database: fact tables without constraints, one reading the new rules refuse.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta VALUES ('schema_version', '5'), ('price_fp', 'old');
             CREATE TABLE seat_accounts (seat TEXT NOT NULL, ts INTEGER NOT NULL, account_uuid TEXT NOT NULL, PRIMARY KEY (seat, ts));
             INSERT INTO seat_accounts VALUES ('work', 100, 'acc-a'), ('work', 200, 'acc-b');
             CREATE TABLE accounts (account_uuid TEXT PRIMARY KEY, email TEXT, display_name TEXT, org_name TEXT, plan TEXT, tier TEXT, seat TEXT, seen_at INTEGER);
             INSERT INTO accounts VALUES ('acc-a', 'a@example.com', NULL, NULL, 'max', 't', 'work', 1);
             CREATE TABLE readings (account_uuid TEXT NOT NULL, kind TEXT NOT NULL, fetched_at INTEGER NOT NULL, utilization REAL NOT NULL, resets_at INTEGER NOT NULL, PRIMARY KEY (account_uuid, kind, fetched_at));
             INSERT INTO readings VALUES ('acc-a', 'seven_day', 1, 40, 9), ('acc-a', 'bogus', 2, 1, 9);
             CREATE TABLE requests (msg_id TEXT PRIMARY KEY, cost REAL NOT NULL DEFAULT -1);
             INSERT INTO requests VALUES ('m', -1);",
        )
        .unwrap();
        assert!(migrate(&c).unwrap(), "the cache is rebuilt");
        let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(n("SELECT COUNT(*) FROM seat_accounts"), 2);
        assert_eq!(n("SELECT COUNT(*) FROM accounts"), 1);
        assert_eq!(n("SELECT COUNT(*) FROM readings"), 1);
        assert_eq!(n("SELECT COUNT(*) FROM requests"), 0);
        assert_eq!(n("SELECT COUNT(*) FROM meta WHERE key = 'price_fp'"), 0);
        assert_eq!(
            meta(&c, "facts_version").unwrap().as_deref(),
            Some(FACTS_VERSION.to_string().as_str())
        );
        // And the migrated readings table carries the constraint now.
        assert!(
            c.execute("INSERT INTO readings VALUES ('a', 'x', 1, 1, 1)", [])
                .is_err()
        );
        // A second open changes nothing.
        assert!(!migrate(&c).unwrap());
        assert_eq!(n("SELECT COUNT(*) FROM seat_accounts"), 2);
    }

    #[test]
    fn a_newer_facts_version_is_refused() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        set_meta(&c, "facts_version", "99").unwrap();
        assert!(migrate(&c).is_err());
    }
}
