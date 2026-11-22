//! The dashboard's cadences, declared once. The battery budget (CLAUDE.md: about 2 CPU-seconds
//! per minute) is kept by this table: per-tick work touches only running sessions, everything
//! heavier runs on its own period. The worker asks `Schedule::due` and reports `Schedule::done`;
//! it holds no intervals of its own.

use std::time::{Duration, Instant};

/// Every periodic piece of the worker's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Tick,
    FullRefresh,
    HeavyParts,
    SystemProcs,
    StatusLine,
    SeatInfo,
    Fleet,
    FleetKeepAlive,
    Analytics,
    AnalyticsFull,
    Limits,
    Efficiency,
    PrevPeriod,
    DaysHeat,
    DiskSizes,
    Power,
}

/// What a job reads, which decides how often it may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    /// Running sessions only: their registry files, pids and new transcript lines.
    Running,
    /// One `stat` or one small file per seat.
    PerSeat,
    /// Index queries over the live agents' sessions (lanes, session totals, recently done).
    LiveTotals,
    /// Queries over a whole period of the index, every transcript file, the vault or a
    /// directory walk.
    Heavy,
    /// The full process table: only while the SYSTEM panel is open.
    System,
}

/// Whether a job runs on the first tick or waits one period (the start-up refresh covered it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Due,
    Waits,
}

#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub job: Job,
    pub name: &'static str,
    pub every: Duration,
    /// The period on battery (eco mode).
    pub eco: Duration,
    pub start: Start,
    pub needs: Needs,
}

const fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// In `Job` order (checked by a test).
#[rustfmt::skip]
pub const JOBS: [Spec; 16] = [
    // One worker tick: running sessions' new lines (`Index::refresh_hot`), their processes,
    // the live snapshot with reused totals, the selected agent's timeline. Three seconds on
    // battery.
    Spec { job: Job::Tick, name: "hot refresh", every: secs(1), eco: secs(3), start: Start::Due, needs: Needs::Running },
    // Every transcript file, for sessions that started or ended between ticks.
    Spec { job: Job::FullRefresh, name: "full index refresh", every: secs(60), eco: secs(60), start: Start::Waits, needs: Needs::Heavy },
    // Lanes, session totals and the recently-done list of the snapshot.
    Spec { job: Job::HeavyParts, name: "lanes and session totals", every: secs(10), eco: secs(10), start: Start::Due, needs: Needs::LiveTotals },
    // The whole process table with CPU, while SYSTEM is open (otherwise only the sessions' pids).
    Spec { job: Job::SystemProcs, name: "process table", every: secs(3), eco: secs(3), start: Start::Due, needs: Needs::System },
    // A fresh status-line reading makes limits exact again: re-read the seats, at most this often.
    Spec { job: Job::StatusLine, name: "status-line readings", every: secs(5), eco: secs(5), start: Start::Waits, needs: Needs::PerSeat },
    // Seats' accounts and limits from `.claude.json` (at once when a login file changes).
    Spec { job: Job::SeatInfo, name: "seat info", every: secs(20), eco: secs(20), start: Start::Waits, needs: Needs::PerSeat },
    // The status line's fleet summary: written when it changed, at most this often…
    Spec { job: Job::Fleet, name: "fleet summary", every: secs(2), eco: secs(2), start: Start::Due, needs: Needs::Running },
    // …and at least this often, so it stays fresh.
    Spec { job: Job::FleetKeepAlive, name: "fleet summary (keep fresh)", every: secs(5), eco: secs(5), start: Start::Due, needs: Needs::Running },
    // An analytics pass: today's and the last hour's spend per project, seats and the vault
    // (sooner when the UI asks or a seat's login changed).
    Spec { job: Job::Analytics, name: "analytics", every: secs(30), eco: secs(30), start: Start::Due, needs: Needs::Heavy },
    // The period aggregates (groups, models, effort, project × model).
    Spec { job: Job::AnalyticsFull, name: "period aggregates", every: secs(120), eco: secs(120), start: Start::Due, needs: Needs::Heavy },
    // Limit analysis per account (sooner on a full pass or a seat change).
    Spec { job: Job::Limits, name: "limits", every: secs(55), eco: secs(55), start: Start::Due, needs: Needs::Heavy },
    Spec { job: Job::Efficiency, name: "efficiency", every: secs(300), eco: secs(300), start: Start::Due, needs: Needs::Heavy },
    // The previous period, for ▲▼ trends.
    Spec { job: Job::PrevPeriod, name: "previous period", every: secs(600), eco: secs(600), start: Start::Due, needs: Needs::Heavy },
    // Daily bars, day × model and the weekday × hour heat map.
    Spec { job: Job::DaysHeat, name: "days and heat map", every: secs(300), eco: secs(300), start: Start::Due, needs: Needs::Heavy },
    // Bytes of every seat's transcripts (SYSTEM).
    Spec { job: Job::DiskSizes, name: "transcript sizes", every: secs(300), eco: secs(300), start: Start::Due, needs: Needs::Heavy },
    // On battery or not (switches eco mode).
    Spec { job: Job::Power, name: "power source", every: secs(30), eco: secs(30), start: Start::Waits, needs: Needs::PerSeat },
];

pub fn spec(job: Job) -> &'static Spec {
    let s = &JOBS[job as usize];
    debug_assert_eq!(s.job, job, "JOBS is out of order");
    s
}

/// `SWARMY_DEBUG=1`: the table, once, at the start.
pub fn print_table() {
    for s in &JOBS {
        eprintln!(
            "job {:<28} every {:>4}s, {:>4}s on battery, {:?}",
            s.name,
            s.every.as_secs(),
            s.eco.as_secs(),
            s.needs
        );
    }
}

/// Frame times of the UI thread: the boot swarm (30 a second for two seconds), animations (5 a
/// second), and eco mode or nothing moving (2 a second).
pub const FRAME_BOOT: Duration = Duration::from_millis(33);
pub const FRAME_ANIMATED: Duration = Duration::from_millis(200);
pub const FRAME_CALM: Duration = Duration::from_millis(500);
/// The shortest wait for input between two frames.
pub const MIN_POLL: Duration = Duration::from_millis(5);
/// `swarmy demo --record` takes 10 frames a second.
pub const FRAME_RECORD: Duration = Duration::from_millis(100);
/// The worker sleeps a tick in steps this long, so a request from the UI is answered sooner.
pub const TICK_STEP: Duration = Duration::from_millis(100);

/// When each job last ran.
pub struct Schedule {
    last: [Option<Instant>; JOBS.len()],
    pub eco: bool,
}

impl Schedule {
    pub fn new(eco: bool) -> Schedule {
        let now = Instant::now();
        Schedule {
            last: JOBS.map(|s| match s.start {
                Start::Due => None,
                Start::Waits => Some(now),
            }),
            eco,
        }
    }

    pub fn period(&self, job: Job) -> Duration {
        let s = spec(job);
        if self.eco { s.eco } else { s.every }
    }

    /// Never ran, or its period has passed.
    pub fn due(&self, job: Job) -> bool {
        self.last[job as usize].is_none_or(|t| t.elapsed() > self.period(job))
    }

    pub fn done(&mut self, job: Job) {
        self.last[job as usize] = Some(Instant::now());
    }

    /// A result computed at `at` by this job is still within its period.
    pub fn fresh(&self, job: Job, at: Instant) -> bool {
        at.elapsed() < self.period(job)
    }

    /// The steps of `TICK_STEP` one tick sleeps.
    pub fn tick_steps(&self) -> u32 {
        (self.period(Job::Tick).as_millis() / TICK_STEP.as_millis()) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_in_job_order_with_unique_names() {
        for (i, s) in JOBS.iter().enumerate() {
            assert_eq!(s.job as usize, i, "{} is out of order", s.name);
        }
        let mut names: Vec<&str> = JOBS.iter().map(|s| s.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), JOBS.len());
    }

    #[test]
    fn heavy_jobs_run_at_most_every_thirty_seconds_and_battery_never_runs_more() {
        for s in &JOBS {
            assert!(
                s.eco >= s.every,
                "{}: eco period shorter than normal",
                s.name
            );
            if s.needs == Needs::Heavy {
                assert!(
                    s.every >= secs(30),
                    "{} is heavy but runs every {:?}",
                    s.name,
                    s.every
                );
            }
        }
        // Only running sessions are touched every tick.
        assert_eq!(spec(Job::Tick).needs, Needs::Running);
    }

    /// The cadences measured against the battery budget (CLAUDE.md); changing one is a decision.
    #[test]
    fn cadences_are_the_measured_ones() {
        let got: Vec<(&str, u64, u64)> = JOBS
            .iter()
            .map(|s| (s.name, s.every.as_secs(), s.eco.as_secs()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("hot refresh", 1, 3),
                ("full index refresh", 60, 60),
                ("lanes and session totals", 10, 10),
                ("process table", 3, 3),
                ("status-line readings", 5, 5),
                ("seat info", 20, 20),
                ("fleet summary", 2, 2),
                ("fleet summary (keep fresh)", 5, 5),
                ("analytics", 30, 30),
                ("period aggregates", 120, 120),
                ("limits", 55, 55),
                ("efficiency", 300, 300),
                ("previous period", 600, 600),
                ("days and heat map", 300, 300),
                ("transcript sizes", 300, 300),
                ("power source", 30, 30),
            ]
        );
    }

    #[test]
    fn a_job_is_due_at_start_or_after_its_period() {
        let mut s = Schedule::new(false);
        assert!(s.due(Job::Analytics));
        assert!(
            !s.due(Job::SeatInfo),
            "waits one period after the start-up refresh"
        );
        s.done(Job::Analytics);
        assert!(!s.due(Job::Analytics));
        s.last[Job::Analytics as usize] = Instant::now().checked_sub(secs(31));
        assert!(s.due(Job::Analytics));
        assert!(s.fresh(Job::Efficiency, Instant::now()));
    }

    #[test]
    fn a_tick_is_one_second_and_three_on_battery() {
        assert_eq!(Schedule::new(false).tick_steps(), 10);
        assert_eq!(Schedule::new(true).tick_steps(), 30);
    }
}
