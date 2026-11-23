//! Account identities in the index.
//!
//! Every `account_uuid` column holds a canonical [`AccountId`]: `<accountUuid>/<organizationUuid>`
//! when the organization is known, else `<accountUuid>`. One Anthropic user (`accountUuid`) can
//! belong to multiple organizations (for example, a personal Max plan and a company Team plan)
//! with separate rate limits, so the pair is the true identity.
//!
//! Older indexes stored bare `<accountUuid>`. [`Index::adopt_accounts`] upgrades them in place on
//! the first refresh that sees organization-scoped accounts:
//! - When a user has a single organization (`1` org for `<account>`), every legacy bare row across
//!   all tables moves to `<account>/<org>` in place with zero data loss and zero transcript
//!   re-reading.
//! - When a user has multiple organizations (`2+` orgs for `<account>`), the legacy bare rows in
//!   `readings` and `accounts` were interleaved across orgs and are removed (preventing a ghost
//!   account and mixed calibration; fresh per-org readings are recorded immediately in the same
//!   refresh). Per-seat `seat_accounts`, `usage_rollup`, and `requests` move to each seat's current
//!   org, and transcripts indexed under the bare id are scheduled for a re-read from offset 0
//!   (respecting `marks` / `rolled_until`) so `owners` recovers `<account>/<org>` from
//!   `session-owner` records.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{Index, READING_REPEAT_MS, SeatInfo, ms};
use crate::core::ids::{AccountId, SeatId};

/// Tables whose `account_uuid` column holds an `AccountId`.
const ACCOUNT_TABLES: &[&str] = &[
    "accounts",
    "readings",
    "seat_accounts",
    "owners",
    "requests",
    "usage_rollup",
    "files",
];

fn has_legacy_bare(conn: &Connection, user_uuid: &str) -> rusqlite::Result<bool> {
    for t in ACCOUNT_TABLES {
        if conn
            .prepare_cached(&format!(
                "SELECT 1 FROM {t} WHERE account_uuid = ?1 LIMIT 1"
            ))?
            .exists([user_uuid])?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Index {
    /// Migrates legacy bare `<accountUuid>` rows to `<accountUuid>/<organizationUuid>` in place.
    /// Returns `true` when multi-org transcripts were scheduled for re-read so `post_pass` should
    /// re-attribute owner-backed requests afterwards.
    pub(super) fn adopt_accounts(&mut self, seat_info: &[SeatInfo]) -> anyhow::Result<bool> {
        let mut sig: Vec<(SeatId, AccountId)> = seat_info
            .iter()
            .filter_map(|si| {
                let id = si.account.as_ref()?.id();
                id.has_org().then(|| (si.seat.clone(), id))
            })
            .collect();
        sig.sort();
        if sig.is_empty() || self.adopted_accounts.as_ref() == Some(&sig) {
            return Ok(false);
        }

        // Fast read-only check: avoid taking a write lock or bumping SQLite's `data_version` when
        // there are no legacy bare rows for any of the current seats' users.
        let users: BTreeSet<&str> = sig.iter().map(|(_, id)| id.user_uuid()).collect();
        let mut any_legacy = false;
        for u in &users {
            if has_legacy_bare(&self.conn, u)? {
                any_legacy = true;
                break;
            }
        }
        if !any_legacy {
            self.adopted_accounts = Some(sig);
            return Ok(false);
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Gather all known `<user>/<org>` identities per user: current seats plus any already-scoped
        // accounts in the database.
        let mut orgs_by_user: BTreeMap<String, BTreeSet<AccountId>> = BTreeMap::new();
        for (_, id) in &sig {
            orgs_by_user
                .entry(id.user_uuid().to_string())
                .or_default()
                .insert(id.clone());
        }
        {
            let mut st = tx.prepare_cached(
                "SELECT account_uuid FROM accounts WHERE instr(account_uuid, '/') > 0",
            )?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            for r in rows {
                let id = AccountId::new(r?);
                if id.has_org() && users.contains(id.user_uuid()) {
                    orgs_by_user
                        .entry(id.user_uuid().to_string())
                        .or_default()
                        .insert(id);
                }
            }
        }

        let mut moved = 0usize;
        let mut reattribute_owners = false;

        for (user, orgs) in &orgs_by_user {
            if !has_legacy_bare(&tx, user)? {
                continue;
            }
            let mut it = orgs.iter();
            match (it.next(), it.next()) {
                (Some(target), None) => {
                    let org_uuid = target.org_uuid();
                    moved += tx.execute(
                        "UPDATE requests SET account_uuid = ?1 WHERE account_uuid = ?2",
                        params![target.as_str(), user],
                    )?;
                    moved += tx.execute(
                    "UPDATE OR IGNORE usage_rollup SET account_uuid = ?1 WHERE account_uuid = ?2",
                    params![target.as_str(), user],
                )?;
                    moved += tx.execute(
                        "UPDATE OR IGNORE owners SET account_uuid = ?1 WHERE account_uuid = ?2",
                        params![target.as_str(), user],
                    )?;
                    tx.execute("DELETE FROM owners WHERE account_uuid = ?1", [user])?;
                    moved += tx.execute(
                        "UPDATE files SET account_uuid = ?1 WHERE account_uuid = ?2",
                        params![target.as_str(), user],
                    )?;
                    moved += tx.execute(
                        "UPDATE seat_accounts SET account_uuid = ?1 WHERE account_uuid = ?2",
                        params![target.as_str(), user],
                    )?;
                    moved += tx.execute(
                        "UPDATE OR IGNORE readings SET account_uuid = ?1 WHERE account_uuid = ?2",
                        params![target.as_str(), user],
                    )?;
                    tx.execute("DELETE FROM readings WHERE account_uuid = ?1", [user])?;
                    moved += tx.execute(
                    "UPDATE OR IGNORE accounts SET account_uuid = ?1, org_uuid = COALESCE(org_uuid, ?3)
                     WHERE account_uuid = ?2",
                    params![target.as_str(), user, org_uuid],
                )?;
                    tx.execute("DELETE FROM accounts WHERE account_uuid = ?1", [user])?;
                }
                (Some(_), Some(_)) => {
                    // Multi-org split: legacy bare readings and account metadata were mixed across orgs.
                    // Remove them so no ghost account remains; fresh readings arrive in `post_pass`.
                    tx.execute("DELETE FROM readings WHERE account_uuid = ?1", [user])?;
                    tx.execute("DELETE FROM accounts WHERE account_uuid = ?1", [user])?;
                    // Schedule any transcript indexed under the bare id (or holding a bare owner) for
                    // re-read from offset 0 (`size + 1` forces `reset = true` on all OSes, and `marks`
                    // skips rolled-up records while repopulating `owners` with `<account>/<org>`).
                    let reset_files = tx.execute(
                        "UPDATE files
                     SET size = size + 1, offset = size + 1, account_uuid = NULL, last_ts = 0
                     WHERE account_uuid = ?1
                        OR (kind = 'main' AND EXISTS (
                              SELECT 1 FROM owners o
                              WHERE o.seat = files.seat AND o.session_id = files.session_id
                                AND o.account_uuid = ?1
                            ))",
                        [user],
                    )?;
                    tx.execute("DELETE FROM owners WHERE account_uuid = ?1", [user])?;
                    // Assign each seat's own historical rows to that seat's current organization.
                    for (seat, target) in &sig {
                        if target.user_uuid() != user {
                            continue;
                        }
                        moved += tx.execute(
                        "UPDATE seat_accounts SET account_uuid = ?1 WHERE seat = ?2 AND account_uuid = ?3",
                        params![target.as_str(), seat, user],
                    )?;
                        moved += tx.execute(
                        "UPDATE OR IGNORE usage_rollup SET account_uuid = ?1 WHERE seat = ?2 AND account_uuid = ?3",
                        params![target.as_str(), seat, user],
                    )?;
                        moved += tx.execute(
                        "UPDATE requests SET account_uuid = ?1 WHERE seat = ?2 AND account_uuid = ?3",
                        params![target.as_str(), seat, user],
                    )?;
                    }
                    if reset_files > 0 {
                        reattribute_owners = true;
                    }
                    moved += reset_files;
                }
                (None, _) => {}
            }
        }

        if moved > 0 {
            super::bump_settled(&tx)?;
        }
        tx.commit()?;
        self.adopted_accounts = Some(sig);
        Ok(reattribute_owners)
    }
}

/// Records current seat accounts, limit readings, and seat login changes inside `post_pass`'s
/// transaction. Returns the `(seat, ts)` changes whose requests from `ts` onward need
/// re-attribution.
pub(super) fn record_seat_facts(
    tx: &Connection,
    seat_info: &[SeatInfo],
    now: i64,
) -> rusqlite::Result<Vec<(SeatId, i64)>> {
    for si in seat_info {
        if let Some(a) = &si.account {
            let id = a.id();
            let plan = a.plan_label();
            let tier = a.effective_tier();
            tx.prepare_cached(
                "INSERT INTO accounts(account_uuid, email, display_name, org_name, org_uuid, plan, tier, seat, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(account_uuid) DO UPDATE SET email = excluded.email, display_name = excluded.display_name,
                   org_name = excluded.org_name, org_uuid = excluded.org_uuid, plan = excluded.plan, tier = excluded.tier, seat = excluded.seat, seen_at = excluded.seen_at
                 WHERE (email, display_name, org_name, org_uuid, plan, tier, seat) IS NOT
                         (excluded.email, excluded.display_name, excluded.org_name, excluded.org_uuid, excluded.plan, excluded.tier, excluded.seat)
                   OR seen_at < excluded.seen_at - 3600000",
            )?
            .execute(params![
                id.as_str(),
                a.email,
                a.display_name,
                a.organization_name,
                a.organization_uuid,
                plan,
                tier,
                si.seat,
                now
            ])?;
        }
        if let Some(u) = &si.utilization {
            for r in &u.readings {
                // A reading that says what one a few minutes away already said (same account,
                // window and value) adds nothing: sources report every few seconds.
                tx.prepare_cached(
                    "INSERT INTO readings(account_uuid, kind, fetched_at, utilization, resets_at)
                     SELECT ?1, ?2, ?3, ?4, ?5 WHERE NOT EXISTS (
                       SELECT 1 FROM readings WHERE account_uuid = ?1 AND kind = ?2
                         AND fetched_at BETWEEN ?3 - ?6 AND ?3 + ?6
                         AND utilization = ?4 AND ABS(resets_at - ?5) < 60000)
                     ON CONFLICT(account_uuid, kind, fetched_at) DO NOTHING",
                )?
                .execute(params![
                    u.account_id.as_str(),
                    r.kind.key(),
                    ms(r.fetched_at),
                    r.utilization,
                    ms(r.resets_at),
                    READING_REPEAT_MS
                ])?;
            }
        }
    }
    // What each seat has logged in now. A change of login is dated when swarmy sees it
    // (review H1): never at the profile's `profileFetchedAt` nor at a reading's time, and never
    // before the seat's newest recorded change, so the history only moves forward.
    let mut reattribute: Vec<(SeatId, i64)> = Vec::new();
    for si in seat_info {
        let Some(a) = &si.account else { continue };
        let acct_id = a.id();
        let newest: Option<(i64, String)> = tx
            .prepare_cached(
                "SELECT ts, account_uuid FROM seat_accounts WHERE seat = ?1 ORDER BY ts DESC LIMIT 1",
            )?
            .query_row(params![si.seat], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        if newest
            .as_ref()
            .is_some_and(|(_, acc)| acc == acct_id.as_str())
        {
            continue;
        }
        let ts = newest.map_or(now, |(t, _)| now.max(t + 1));
        tx.prepare_cached("INSERT INTO seat_accounts(seat, ts, account_uuid) VALUES (?1, ?2, ?3)")?
            .execute(params![si.seat, ts, acct_id.as_str()])?;
        reattribute.push((si.seat.clone(), ts));
    }
    Ok(reattribute)
}
