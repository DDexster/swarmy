//! The index schema. The index is a cache of what the transcripts say: when the schema version
//! changes it is dropped and rebuilt from the transcripts.

pub const VERSION: &str = "5";

pub const SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS files (
  path TEXT PRIMARY KEY,
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  ino INTEGER NOT NULL DEFAULT 0,
  size INTEGER NOT NULL DEFAULT 0,
  mtime INTEGER NOT NULL DEFAULT 0,
  offset INTEGER NOT NULL DEFAULT 0,
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
  fast INTEGER NOT NULL DEFAULT 0,
  cwd TEXT,
  input INTEGER NOT NULL DEFAULT 0,
  cw5 INTEGER NOT NULL DEFAULT 0,
  cw1h INTEGER NOT NULL DEFAULT 0,
  cread INTEGER NOT NULL DEFAULT 0,
  output INTEGER NOT NULL DEFAULT 0,
  thinking INTEGER NOT NULL DEFAULT 0,
  web INTEGER NOT NULL DEFAULT 0,
  tools INTEGER NOT NULL DEFAULT 0,
  cost REAL NOT NULL DEFAULT -1,
  priced INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS requests_ts ON requests(ts);
CREATE INDEX IF NOT EXISTS requests_agent ON requests(seat, session_id, agent_id, ts);
CREATE INDEX IF NOT EXISTS requests_cost ON requests(cost);

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
  is_error INTEGER NOT NULL DEFAULT 0
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
  compactions INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (seat, session_id)
);

CREATE TABLE IF NOT EXISTS agents (
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  workflow_run TEXT,
  agent_type TEXT,
  description TEXT,
  tool_use_id TEXT,
  parent_agent_id TEXT,
  depth INTEGER,
  requested_model TEXT,
  effort TEXT,
  finished INTEGER NOT NULL DEFAULT 0,
  first_ts INTEGER,
  last_ts INTEGER,
  PRIMARY KEY (seat, session_id, agent_id)
);

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
  kind TEXT NOT NULL,
  fetched_at INTEGER NOT NULL,
  utilization REAL NOT NULL,
  resets_at INTEGER NOT NULL,
  PRIMARY KEY (account_uuid, kind, fetched_at)
);

CREATE TABLE IF NOT EXISTS prompts (
  seat TEXT NOT NULL,
  session_id TEXT NOT NULL,
  ts INTEGER NOT NULL,
  chars INTEGER NOT NULL
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
  total_usd REAL NOT NULL,
  models TEXT NOT NULL,
  PRIMARY KEY (seat, session_id)
);

CREATE TABLE IF NOT EXISTS record_types (
  type TEXT PRIMARY KEY,
  count INTEGER NOT NULL
);
"#;

pub const TABLES: &[&str] = &[
    "files",
    "requests",
    "tool_calls",
    "sessions",
    "agents",
    "owners",
    "projects",
    "accounts",
    "prompts",
    "api_errors",
    "cost_states",
    "record_types",
];

/// Indexes added after a schema version shipped; created on every open, cheaply.
pub const EXTRA_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS requests_account_ts ON requests(account_uuid, ts);
CREATE INDEX IF NOT EXISTS requests_no_account ON requests(seat, session_id) WHERE account_uuid IS NULL;
CREATE INDEX IF NOT EXISTS files_mtime ON files(mtime);
"#;
