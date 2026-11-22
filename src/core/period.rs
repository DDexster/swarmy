//! Reporting periods.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};

#[derive(Debug, Clone, PartialEq)]
pub enum Period {
    Today,
    Yesterday,
    /// The last N hours.
    Hours(i64),
    /// The last N days, counted back from now.
    Days(i64),
    /// The current calendar month.
    Month,
    /// The current weekly-limit window of an account (resolved by the caller; 7 days otherwise).
    LimitWeek,
    All,
    Range(NaiveDate, NaiveDate),
}

impl std::str::FromStr for Period {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        let num = |t: &str| t.parse::<i64>().ok().filter(|n| *n > 0);
        Ok(match s.as_str() {
            "today" => Period::Today,
            "yesterday" => Period::Yesterday,
            "week" | "limit" | "limit-week" => Period::LimitWeek,
            "month" => Period::Month,
            "all" => Period::All,
            _ => {
                if let Some(n) = s.strip_suffix('d').and_then(num) {
                    Period::Days(n)
                } else if let Some(n) = s.strip_suffix('h').and_then(num) {
                    Period::Hours(n)
                } else if let Some((a, b)) = s.split_once("..") {
                    Period::Range(parse_day(a)?, parse_day(b)?)
                } else if let Ok(d) = parse_day(&s) {
                    Period::Range(d, d)
                } else {
                    return Err(format!(
                        "unknown period '{s}': use today, yesterday, week, month, all, 24h, 7d, 30d, YYYY-MM-DD or FROM..TO"
                    ));
                }
            }
        })
    }
}

fn parse_day(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| format!("bad date '{s}', expected YYYY-MM-DD"))
}

/// A half-open UTC interval `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Span {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Span {
    pub fn contains(&self, t: DateTime<Utc>) -> bool {
        t >= self.start && t < self.end
    }

    pub fn hours(&self) -> f64 {
        (self.end - self.start).num_seconds() as f64 / 3600.0
    }
}

fn local_midnight(d: NaiveDate) -> DateTime<Utc> {
    let naive = d.and_hms_opt(0, 0, 0).unwrap();
    Local
        .from_local_datetime(&naive)
        .earliest()
        .unwrap_or_else(|| Local.from_utc_datetime(&naive))
        .with_timezone(&Utc)
}

impl Period {
    /// Resolves to a UTC span in the local calendar. `limit_week` is the current weekly window
    /// when the caller knows it.
    pub fn span(&self, now: DateTime<Utc>, limit_week: Option<Span>) -> Span {
        let today = now.with_timezone(&Local).date_naive();
        let end = now + Duration::seconds(1);
        match self {
            Period::Today => Span {
                start: local_midnight(today),
                end,
            },
            Period::Yesterday => Span {
                start: local_midnight(today - Duration::days(1)),
                end: local_midnight(today),
            },
            Period::Hours(h) => Span {
                start: now - Duration::hours(*h),
                end,
            },
            Period::Days(d) => Span {
                start: now - Duration::days(*d),
                end,
            },
            Period::Month => Span {
                start: local_midnight(today.with_day(1).unwrap()),
                end,
            },
            Period::LimitWeek => limit_week.unwrap_or(Span {
                start: now - Duration::days(7),
                end,
            }),
            Period::All => Span {
                start: DateTime::<Utc>::MIN_UTC,
                end,
            },
            Period::Range(a, b) => Span {
                start: local_midnight(*a),
                end: local_midnight(*b + Duration::days(1)),
            },
        }
    }

    pub fn label(&self) -> String {
        match self {
            Period::Today => "today".into(),
            Period::Yesterday => "yesterday".into(),
            Period::Hours(h) => format!("last {h}h"),
            Period::Days(d) => format!("last {d}d"),
            Period::Month => "this month".into(),
            Period::LimitWeek => "limit week".into(),
            Period::All => "all time".into(),
            Period::Range(a, b) if a == b => a.to_string(),
            Period::Range(a, b) => format!("{a}..{b}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!("7d".parse::<Period>().unwrap(), Period::Days(7));
        assert_eq!("24h".parse::<Period>().unwrap(), Period::Hours(24));
        assert_eq!("week".parse::<Period>().unwrap(), Period::LimitWeek);
        let d = |m, dd| NaiveDate::from_ymd_opt(2026, m, dd).unwrap();
        assert_eq!(
            "2026-11-01..2026-11-22".parse::<Period>().unwrap(),
            Period::Range(d(11, 1), d(11, 22))
        );
        assert_eq!(
            "2026-11-20".parse::<Period>().unwrap(),
            Period::Range(d(11, 20), d(11, 20))
        );
        assert!("fortnight".parse::<Period>().is_err());
        assert!("0d".parse::<Period>().is_err());
    }

    #[test]
    fn range_is_inclusive_of_the_last_day() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let s = Period::Range(d, d).span(Utc::now(), None);
        assert_eq!((s.end - s.start).num_hours(), 24);
    }
}
