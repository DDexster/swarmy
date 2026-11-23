//! One analytics pass: the period aggregates, projects, seats and limits, reusing whatever
//! earlier passes computed that is still within its cadence (`jobs::JOBS`).

use std::time::Instant;

use chrono::Utc;

use super::jobs::{Job, Schedule};
use super::{Analytics, ProjectRow, SeatRow, Wants, debug};
use crate::app::context::Ctx;
use crate::claude::registry;
use crate::core::period::{Period, Span};
use crate::index::Index;
use crate::index::query::{Dim, Filter, Row};
use crate::limits::{self, AccountReport};
use crate::vault::Vault;

/// Day bars, the weekday × hour heat map and day × model spend (`Job::DaysHeat`).
pub type SlowParts = (
    Vec<Row>,
    Vec<(String, String, f64)>,
    Vec<(String, String, f64)>,
);

/// What one pass reads, and what it may reuse from earlier passes.
pub struct Inputs<'a> {
    pub ctx: &'a Ctx,
    pub idx: &'a Index,
    pub wants: &'a Wants,
    pub jobs: &'a Schedule,
    /// Bytes of each seat's transcripts.
    pub disk: &'a [(String, u64)],
    /// The index's outlook (read with the transcript sizes).
    pub storage: Option<crate::core::storage::Outlook>,
    pub slow: Option<SlowParts>,
    /// The last analytics when this is not a full pass: period aggregates are reused from it.
    pub reuse_period: Option<&'a Analytics>,
    /// The last limits when they are not due.
    pub reuse_limits: Option<&'a [AccountReport]>,
    /// The last analytics whatever the cadence: slow parts are reused from it while fresh.
    pub last: Option<&'a Analytics>,
}

pub fn analytics(i: Inputs) -> anyhow::Result<Analytics> {
    let Inputs {
        ctx,
        idx,
        wants: w,
        jobs,
        disk,
        storage,
        slow,
        reuse_period: prev,
        reuse_limits: prev_limits,
        last,
    } = i;
    let t_start = Instant::now();
    let (days_cached, heat_cached, days_model) = slow.unwrap_or_default();
    let period = w.period.clone().unwrap_or(Period::Days(7));
    let span = period.span(ctx.now, ctx.limit_week(idx, None)?);
    let f = Filter::default();
    let group = w.group.unwrap_or(Dim::Project);
    let reuse = prev.filter(|p| p.period.as_ref() == Some(&period) && p.group == Some(group));
    let same = last.filter(|p| p.period.as_ref() == Some(&period));
    let eff_reuse = same.filter(|p| {
        p.efficiency.is_some()
            && p.efficiency_at
                .is_some_and(|t| jobs.fresh(Job::Efficiency, t))
    });
    let prev_reuse = same.filter(|p| p.prev_at.is_some_and(|t| jobs.fresh(Job::PrevPeriod, t)));
    let mut groups = match reuse {
        Some(p) => p.groups.clone(),
        None => idx.spend_by(group, span, &f)?,
    };
    if group == Dim::Account {
        for r in &mut groups {
            r.label = ctx.account_name(&r.key);
        }
    }
    let mut vault_logins = Vec::new();
    let mut seats = Vec::new();
    let mut states = std::collections::BTreeMap::new();
    let mut problems = ctx.problems.clone();
    // An unreadable vault is said so in ATTENTION, never shown as an empty one.
    match Vault::open(&ctx.dirs.vault_dir()) {
        Ok(mut v) => {
            match v.edit(|tx| tx.seat_states(&ctx.seats)) {
                Ok(s) => states = s,
                Err(e) => problems.push(format!("vault: seats not checked: {e:#}")),
            }
            vault_logins = v.logins().to_vec();
        }
        Err(e) => problems.push(format!("vault unreadable: {e:#}")),
    }
    problems.dedup();
    for (s, si) in ctx.seats.iter().zip(&ctx.seat_info) {
        let state = states.get(&s.id).cloned().unwrap_or_default();
        seats.push(SeatRow {
            id: s.id.clone(),
            label: s.label.clone(),
            account: if state.unverified_since.is_some() {
                None
            } else {
                si.account.as_ref().map(|a| a.account_uuid.clone())
            },
            login: state.saved_as.clone().or_else(|| {
                vault_logins
                    .iter()
                    .find(|l| l.mounted.as_ref() == Some(&s.id))
                    .map(|l| l.label.clone())
            }),
            live: registry::live_sessions(&s.id, &s.sessions_dir(), &ctx.procs).len(),
            state,
        });
    }
    let dbg = debug();
    macro_rules! timed {
        ($name:expr, $e:expr) => {{
            let t = Instant::now();
            let v = $e;
            if dbg {
                eprintln!("  {} {:?}", $name, t.elapsed());
            }
            v
        }};
    }
    let midnight = chrono::Local::now()
        .date_naive()
        .and_time(chrono::NaiveTime::MIN)
        .and_local_timezone(chrono::Local)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(ctx.now);
    let end = ctx.now + chrono::Duration::seconds(1);
    // The period before, of the same length, for trends.
    let prev_span = (!matches!(period, Period::All)).then(|| span.previous());
    let mut projects: Vec<ProjectRow> = match reuse {
        Some(prev) => prev
            .projects
            .iter()
            .map(|p| ProjectRow {
                key: p.key.clone(),
                name: p.name.clone(),
                period: p.period,
                requests: p.requests,
                prev: p.prev,
                ..Default::default()
            })
            .collect(),
        None => Vec::new(),
    };
    {
        let mut upsert = |rows: Vec<Row>, set: &dyn Fn(&mut ProjectRow, &Row)| {
            for r in rows {
                match projects.iter_mut().find(|p| p.key == r.key) {
                    Some(p) => set(p, &r),
                    None => {
                        let mut p = ProjectRow {
                            key: r.key.clone(),
                            name: r.label.clone(),
                            ..Default::default()
                        };
                        set(&mut p, &r);
                        projects.push(p);
                    }
                }
            }
        };
        if reuse.is_none() {
            upsert(idx.spend_by(Dim::Project, span, &f)?, &|p, r| {
                p.period = r.cost;
                p.requests = r.requests;
            });
            match (prev_reuse, prev_span) {
                (Some(l), _) => {
                    let rows: Vec<Row> = l
                        .projects
                        .iter()
                        .filter(|p| p.prev > 0.0)
                        .map(|p| Row {
                            key: p.key.clone(),
                            label: p.name.clone(),
                            requests: 0,
                            usage: Default::default(),
                            cost: p.prev,
                            unpriced: 0,
                            pending: 0,
                        })
                        .collect();
                    upsert(rows, &|p, r| p.prev = r.cost);
                }
                (None, Some(ps)) => upsert(
                    timed!("prev_projects", idx.spend_by(Dim::Project, ps, &f)?),
                    &|p, r| p.prev = r.cost,
                ),
                (None, None) => {}
            }
        }
        upsert(
            idx.spend_by(
                Dim::Project,
                Span {
                    start: midnight,
                    end,
                },
                &f,
            )?,
            &|p, r| p.today = r.cost,
        );
        upsert(
            idx.spend_by(
                Dim::Project,
                Span {
                    start: ctx.now - chrono::Duration::hours(1),
                    end,
                },
                &f,
            )?,
            &|p, r| p.hour = r.cost,
        );
    }
    if dbg {
        eprintln!("  projects+vault {:?}", t_start.elapsed());
    }
    // Projects seen only in the previous period stay out of the list.
    projects.retain(|p| p.period > 0.0 || p.today > 0.0 || p.hour > 0.0);
    projects.sort_by(|a, b| {
        b.hour
            .total_cmp(&a.hour)
            .then(b.today.total_cmp(&a.today))
            .then(b.period.total_cmp(&a.period))
    });
    Ok(Analytics {
        projects,
        period: Some(period),
        group: Some(group),
        groups,
        by_model: match reuse {
            Some(p) => p.by_model.clone(),
            None => timed!("by_model", idx.spend_by(Dim::Model, span, &f)?),
        },
        total: match reuse {
            Some(p) => p.total.clone(),
            None => timed!("total", Some(idx.total(span, &f)?)),
        },
        days: days_cached,
        days_model,
        heat: heat_cached,
        efficiency: match eff_reuse {
            Some(p) => p.efficiency.clone(),
            None => timed!("efficiency", idx.efficiency(span).ok()),
        },
        efficiency_at: match eff_reuse {
            Some(p) => p.efficiency_at,
            None => Some(Instant::now()),
        },
        prev_at: match prev_reuse.or(reuse) {
            Some(p) => p.prev_at,
            None => Some(Instant::now()),
        },
        prev_total: match prev_reuse.or(reuse) {
            Some(p) => p.prev_total,
            None => match prev_span {
                Some(ps) => Some(timed!("prev_total", idx.total(ps, &f)?.cost)),
                None => None,
            },
        },
        subscriptions: ctx
            .config
            .subscription
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
        prev_models: match prev_reuse.or(reuse) {
            Some(p) => p.prev_models.clone(),
            None => match prev_span {
                Some(ps) => timed!("prev_models", idx.spend_by(Dim::Model, ps, &f)?)
                    .into_iter()
                    .map(|r| (r.key, r.cost))
                    .collect(),
                None => Vec::new(),
            },
        },
        effort: match reuse {
            Some(p) => p.effort.clone(),
            None => timed!("effort", idx.matrix(Dim::Model, Dim::Effort, span, &f)?),
        },
        substitutions: match reuse {
            Some(p) => p.substitutions.clone(),
            None => timed!("subs", idx.model_substitutions(span, &f)?),
        },
        project_model: match reuse {
            Some(p) => p.project_model.clone(),
            None => timed!(
                "proj×model",
                idx.matrix(Dim::Project, Dim::Model, span, &f)?
            ),
        },
        limits: match prev_limits {
            Some(l) => l.to_vec(),
            None => timed!("limits", limits::all_reports(ctx, idx)?),
        },
        logins: vault_logins,
        seats,
        problems,
        transcripts_bytes: disk.to_vec(),
        index_bytes: std::fs::metadata(ctx.dirs.index_file())
            .map(|m| m.len())
            .unwrap_or(0),
        storage,
        storage_budget: ctx.config.storage.budget_mb << 20,
    })
}
