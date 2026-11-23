//! Read marks: what the writer must not read again because it was rolled up already
//! (`docs/design/storage.md` §4.3).
//!
//! After a compaction the rolled-up records' keys (`msg_id`, tool ids) are gone, so a record read
//! again would be inserted as new and counted twice against its rollup. Bytes past a file's
//! stored offset were never read, so an incremental read is always new. A file read from its
//! start (replaced, truncated, found again, or every file after a cache rebuild) may repeat
//! records the index already rolled up: those older than `rolled_until` are skipped when the
//! file belongs to what was indexed before ([`Marks::applies`], the one rule).

use std::collections::HashSet;

use rusqlite::Connection;

use super::compact::canonical;
use crate::claude::files::TranscriptFile;
use crate::claude::seats::{Seat, SeatId};

/// The seats a compaction ran over, for one refresh.
pub(super) struct Marks {
    /// The seats whose config dir is in `rollup_seats`.
    rolled_seats: HashSet<SeatId>,
}

impl Marks {
    pub(super) fn load(conn: &Connection, seats: &[Seat]) -> rusqlite::Result<Marks> {
        let dirs: HashSet<String> = conn
            .prepare_cached("SELECT seat_dir FROM rollup_seats")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let rolled_seats = if dirs.is_empty() {
            HashSet::new()
        } else {
            seats
                .iter()
                .filter(|s| dirs.contains(&canonical(&s.dir)))
                .map(|s| s.id.clone())
                .collect()
        };
        Ok(Marks { rolled_seats })
    }

    /// Whether `file`, read from byte `start`, may repeat rolled-up records, so that its records
    /// older than `rolled_until` must be skipped. Decided before the refresh writes anything (a
    /// session first seen in this refresh is new information however its files are ordered):
    /// 1. it is read from its start, and
    /// 2. its session was indexed before (a row in `agents`, which outlives the raw rows): a
    ///    known file read again, a resumed session, a subagent of a known session, a seat whose
    ///    dir was moved or relabelled (session ids stay); or
    /// 3. its seat was indexed when a compaction ran: a new file of a known seat copying old
    ///    records, such as a forked session (new session id, old message ids and times).
    ///
    /// Otherwise (a seat found for the first time, sessions never seen) the records are new; they
    /// go into raw rows and the next compaction adds them to the rollups.
    pub(super) fn applies(
        &self,
        conn: &Connection,
        file: &TranscriptFile,
        start: u64,
    ) -> rusqlite::Result<bool> {
        if start > 0 {
            return Ok(false);
        }
        if self.rolled_seats.contains(&file.seat) {
            return Ok(true);
        }
        conn.prepare_cached("SELECT 1 FROM agents WHERE session_id = ?1 LIMIT 1")?
            .exists([&file.session_id])
    }
}

/// Whether a record at `ts` of a file whose marks apply (see [`Marks::applies`]) was rolled up
/// already and is skipped.
pub(super) fn rolled_up(applies: bool, rolled_until: i64, ts: i64) -> bool {
    applies && ts < rolled_until
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::files::FileKind;
    use crate::index::schema;

    fn file(seat: &Seat, session: &str) -> TranscriptFile {
        TranscriptFile {
            path: format!("/x/{session}.jsonl").into(),
            seat: seat.id.clone(),
            session_id: session.into(),
            agent_id: String::new(),
            kind: FileKind::Main,
        }
    }

    #[test]
    fn the_rule() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("old")).unwrap();
        let seat = |label: &str| {
            let dir = t.path().join(label);
            Seat::new(label, dir.clone(), dir.join(".claude.json"), false)
        };
        let seats = [seat("old"), seat("new")];
        let (old, new) = (&seats[0], &seats[1]);
        let c = Connection::open_in_memory().unwrap();
        schema::migrate(&c).unwrap();
        c.execute_batch(
            "INSERT INTO agents(seat, session_id, agent_id, kind) VALUES ('elsewhere', 'known', '', 'main');",
        )
        .unwrap();
        // Nothing compacted yet: only known sessions.
        let m = Marks::load(&c, &seats).unwrap();
        assert!(m.applies(&c, &file(new, "known"), 0).unwrap());
        assert!(!m.applies(&c, &file(old, "fork"), 0).unwrap());
        c.execute(
            "INSERT INTO rollup_seats VALUES (?1, 'old')",
            [canonical(&seats[0].dir)],
        )
        .unwrap();
        let m = Marks::load(&c, &seats).unwrap();
        // A known session (on any seat), a compacted seat, never past the stored offset.
        assert!(m.applies(&c, &file(new, "known"), 0).unwrap());
        assert!(m.applies(&c, &file(old, "fork"), 0).unwrap());
        assert!(!m.applies(&c, &file(new, "fresh"), 0).unwrap());
        assert!(!m.applies(&c, &file(old, "known"), 10).unwrap());
        assert!(rolled_up(true, 100, 99));
        assert!(!rolled_up(true, 100, 100));
        assert!(!rolled_up(false, 100, 99));
    }
}
