//! Budget periods, shared by the proxy (which enforces caps) and the admin API
//! (which reports how much of a cap is used), so the two can never disagree
//! on which counters a period reads.

use chrono::{DateTime, Datelike, TimeZone, Utc};

/// The key that namespaces a budget's usage counters for the period `now`
/// falls in, or `None` when no cap is set. `monthly` rolls over at each
/// calendar month in `timezone`; `term` resets whenever `budget_started_at`
/// changes; `lifetime` (and any unknown value) never resets.
pub fn period_key(
    budget_tokens: Option<i64>,
    budget_cost_usd: Option<f64>,
    budget_period: Option<&str>,
    budget_started_at: Option<DateTime<Utc>>,
    timezone: &str,
    now: DateTime<Utc>,
) -> Option<String> {
    if budget_tokens.is_none() && budget_cost_usd.is_none() {
        return None;
    }
    let key = match budget_period.unwrap_or("lifetime") {
        "monthly" => {
            let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
            let local = now.with_timezone(&tz);
            format!("m:{}-{:02}", local.year(), local.month())
        }
        "term" => format!(
            "t:{}",
            budget_started_at.map(|t| t.timestamp()).unwrap_or(0)
        ),
        _ => format!(
            "l:{}",
            budget_started_at.map(|t| t.timestamp()).unwrap_or(0)
        ),
    };
    Some(key)
}

/// When the period `now` falls in began and, for a monthly budget, when it
/// ends: the first of this month and of the next, at midnight in `timezone`.
/// A term or lifetime budget runs from `budget_started_at` and never resets.
pub fn period_bounds(
    budget_period: Option<&str>,
    budget_started_at: Option<DateTime<Utc>>,
    timezone: &str,
    now: DateTime<Utc>,
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    if budget_period != Some("monthly") {
        return (budget_started_at, None);
    }
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
    let local = now.with_timezone(&tz);
    let start = tz
        .with_ymd_and_hms(local.year(), local.month(), 1, 0, 0, 0)
        .earliest()
        .map(|t| t.with_timezone(&Utc));
    let (y, m) = if local.month() == 12 {
        (local.year() + 1, 1)
    } else {
        (local.year(), local.month() + 1)
    };
    let end = tz
        .with_ymd_and_hms(y, m, 1, 0, 0, 0)
        .earliest()
        .map(|t| t.with_timezone(&Utc));
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn no_cap_no_counters() {
        assert_eq!(
            period_key(
                None,
                None,
                Some("monthly"),
                None,
                "UTC",
                at("2026-09-28T00:00:00Z")
            ),
            None
        );
    }

    #[test]
    fn a_month_is_the_tenants_month() {
        // 2026-10-01 05:00 UTC is still September 30 in Phoenix (UTC-7).
        let now = at("2026-10-01T05:00:00Z");
        assert_eq!(
            period_key(
                None,
                Some(5.0),
                Some("monthly"),
                None,
                "America/Phoenix",
                now
            )
            .as_deref(),
            Some("m:2026-09")
        );
        assert_eq!(
            period_key(None, Some(5.0), Some("monthly"), None, "UTC", now).as_deref(),
            Some("m:2026-10")
        );
        let (start, end) = period_bounds(Some("monthly"), None, "America/Phoenix", now);
        assert_eq!(start, Some(at("2026-09-01T07:00:00Z")));
        assert_eq!(end, Some(at("2026-10-01T07:00:00Z")));
    }

    #[test]
    fn december_rolls_into_january() {
        let (_, end) = period_bounds(Some("monthly"), None, "UTC", at("2026-12-15T00:00:00Z"));
        assert_eq!(end, Some(at("2027-01-01T00:00:00Z")));
    }

    #[test]
    fn term_and_lifetime_run_from_their_start() {
        let started = at("2026-08-17T00:00:00Z");
        assert_eq!(
            period_key(
                Some(1),
                None,
                Some("term"),
                Some(started),
                "UTC",
                at("2026-09-28T00:00:00Z")
            )
            .as_deref(),
            Some(&*format!("t:{}", started.timestamp()))
        );
        assert_eq!(
            period_key(Some(1), None, None, None, "UTC", at("2026-09-28T00:00:00Z")).as_deref(),
            Some("l:0")
        );
        assert_eq!(
            period_bounds(
                Some("term"),
                Some(started),
                "UTC",
                at("2026-09-28T00:00:00Z")
            ),
            (Some(started), None)
        );
    }
}
