//! Learning metrics per task type and ISO week (design section 15 "Metrics";
//! Requirement 17.5; task 25.2).
//!
//! [`SqliteLearning::metrics`] groups the closed requests of `learn_runs` by task type and
//! by the ISO week (Monday to Sunday, UTC) in which they ended. Each group reports:
//! - `requests`: closed requests, and `verified`: those with status `verified`;
//! - `verified_rate`: `verified / requests`;
//! - `mean_model_calls`, `mean_cost_usd` and `mean_cached_tokens` per request.
//!
//! The design also names a cache-hit ratio (cached prompt tokens over all prompt tokens).
//! The Host records only the provider-reported cached tokens per request
//! (`ui_request_budgets.cached_tokens`, Requirement 11.7), not the prompt token total, and
//! providers disagree on whether their input count includes cached tokens (OpenAI's
//! `prompt_tokens` does, Anthropic's `input_tokens` does not). So the metrics report the
//! mean cached tokens per request instead of a ratio.
//!
//! The window is the last `weeks` ISO weeks including the current one (default
//! [`DEFAULT_METRICS_WEEKS`], at most [`MAX_METRICS_WEEKS`]). Groups are ordered by task
//! type (requests without one first) and week, at most [`MAX_METRICS_ROWS`].
use crate::store::{now_ms, sql, SqliteLearning};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::io;

/// Weeks reported when the caller names none.
pub const DEFAULT_METRICS_WEEKS: u32 = 12;
/// Most weeks reported.
pub const MAX_METRICS_WEEKS: u32 = 104;
/// Most groups returned.
pub const MAX_METRICS_ROWS: usize = 10_000;
const DAY_MS: i64 = 24 * 3600 * 1000;

/// Metrics of one task type in one ISO week.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeeklyMetrics {
    /// `None` for requests without a task type.
    pub task_type: Option<String>,
    /// ISO week, for example `2025-W46`.
    pub week: String,
    /// The week's Monday, `YYYY-MM-DD` (UTC).
    pub week_start: String,
    pub requests: u64,
    pub verified: u64,
    pub verified_rate: f64,
    pub mean_model_calls: f64,
    pub mean_cost_usd: f64,
    pub mean_cached_tokens: f64,
}

/// Observational wall-clock measurements, grouped by agent, task type and week.
/// These cohorts are not controlled benchmarks: model, task difficulty, human waits,
/// and other conditions may differ. Only verified runs contribute elapsed samples.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeeklySpeedMetrics {
    pub agent: String,
    pub task_type: String,
    pub week: String,
    pub week_start: String,
    pub requests: u64,
    pub verified: u64,
    pub verified_rate: f64,
    pub elapsed_samples: u64,
    pub median_elapsed_ms: Option<f64>,
    pub p95_elapsed_ms: Option<u64>,
    pub mean_model_calls: f64,
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date of a day number (days since 1970-01-01).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The Monday (day number) of the ISO week holding day number `day`.
fn monday(day: i64) -> i64 {
    // 1970-01-01 was a Thursday.
    day - (day + 3).rem_euclid(7)
}

/// The ISO year and week of the week starting on day number `monday`.
fn iso_week_of_monday(monday: i64) -> (i64, u32) {
    let thursday = monday + 3;
    let (year, _, _) = civil_from_days(thursday);
    let week = (thursday - days_from_civil(year, 1, 1)) / 7 + 1;
    (year, week as u32)
}

/// The ISO week label (`YYYY-Www`) of a Unix time in milliseconds, in UTC.
pub fn iso_week(at_ms: u64) -> String {
    let (year, week) = iso_week_of_monday(monday((at_ms / DAY_MS as u64) as i64));
    format!("{year}-W{week:02}")
}

impl SqliteLearning {
    /// Observational elapsed-time cohorts for named task types and agents. Timings
    /// include user waits and offline time; a trend is not proof of a speedup.
    pub fn speed_metrics(&self, weeks: u32, task_type: Option<&str>) -> io::Result<Vec<WeeklySpeedMetrics>> {
        self.speed_metrics_at(weeks, task_type, now_ms())
    }

    /// [`Self::speed_metrics`] with an explicit observation time, for reproducible tests.
    pub fn speed_metrics_at(&self, weeks: u32, task_type: Option<&str>, now: u64) -> io::Result<Vec<WeeklySpeedMetrics>> {
        let weeks = i64::from(weeks.clamp(1, MAX_METRICS_WEEKS));
        let today = i64::try_from(now / DAY_MS as u64).unwrap_or(i64::MAX / DAY_MS);
        let since = (monday(today) - (weeks - 1) * 7).max(0) * DAY_MS;
        let db = self.lock()?;
        let mut query = db.prepare(
            "SELECT agent,task_type,(ended/86400000)-((ended/86400000+3)%7) AS monday,
                    status,ended-started,model_calls FROM learn_runs
             WHERE ended>=?1 AND ended<=?2 AND task_type IS NOT NULL AND task_type<>''
               AND (?3 IS NULL OR task_type=?3)
             ORDER BY agent,task_type,monday,ended,request_id"
        ).map_err(sql)?;
        let rows = query.query_map(params![since, i64::try_from(now).unwrap_or(i64::MAX), task_type], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?))
        }).map_err(sql)?;
        let mut groups: Vec<(WeeklySpeedMetrics, Vec<u64>, f64)> = Vec::new();
        for row in rows {
            let (agent, task_type, monday, status, elapsed, calls) = row.map_err(sql)?;
            let (year, week) = iso_week_of_monday(monday);
            let label = format!("{year}-W{week:02}");
            if groups.last().is_none_or(|(group, _, _)| group.agent != agent || group.task_type != task_type || group.week != label) {
                if groups.len() == MAX_METRICS_ROWS { break; }
                let (y, m, d) = civil_from_days(monday);
                groups.push((WeeklySpeedMetrics {
                    agent, task_type, week: label, week_start: format!("{y:04}-{m:02}-{d:02}"),
                    requests: 0, verified: 0, verified_rate: 0.0, elapsed_samples: 0,
                    median_elapsed_ms: None, p95_elapsed_ms: None, mean_model_calls: 0.0,
                }, Vec::new(), 0.0));
            }
            let (group, elapsed_samples, total_calls) = groups.last_mut().expect("group created");
            group.requests += 1;
            *total_calls += calls.max(0) as f64;
            if status == "verified" {
                group.verified += 1;
                if elapsed >= 0 { elapsed_samples.push(elapsed as u64); }
            }
        }
        Ok(groups.into_iter().map(|(mut group, mut elapsed, calls)| {
            group.verified_rate = group.verified as f64 / group.requests as f64;
            group.mean_model_calls = calls / group.requests as f64;
            elapsed.sort_unstable();
            let n = elapsed.len();
            group.elapsed_samples = n as u64;
            if n > 0 {
                group.median_elapsed_ms = Some(if n % 2 == 1 { elapsed[n / 2] as f64 }
                    else { (elapsed[n / 2 - 1] as f64 + elapsed[n / 2] as f64) / 2.0 });
                group.p95_elapsed_ms = Some(elapsed[(n * 95).div_ceil(100) - 1]);
            }
            group
        }).collect())
    }
    /// Metrics of the last `weeks` ISO weeks at the current time, for `task_type` or for
    /// every task type. See the module documentation.
    pub fn metrics(&self, weeks: u32, task_type: Option<&str>) -> io::Result<Vec<WeeklyMetrics>> {
        self.metrics_at(weeks, task_type, now_ms())
    }

    /// [`Self::metrics`] at time `now` (Unix ms). `weeks` is clamped to
    /// `1..=MAX_METRICS_WEEKS`.
    pub fn metrics_at(
        &self,
        weeks: u32,
        task_type: Option<&str>,
        now: u64,
    ) -> io::Result<Vec<WeeklyMetrics>> {
        let weeks = i64::from(weeks.clamp(1, MAX_METRICS_WEEKS));
        let today = i64::try_from(now / DAY_MS as u64).unwrap_or(i64::MAX / DAY_MS);
        let first = monday(today) - (weeks - 1) * 7;
        let since = first.max(0) * DAY_MS;
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT task_type,(ended/86400000)-((ended/86400000+3)%7) AS monday,COUNT(*),
                 COALESCE(SUM(status='verified'),0),AVG(model_calls),AVG(cost_usd),AVG(cached_tokens)
                 FROM learn_runs WHERE ended>=?1 AND (?2 IS NULL OR task_type=?2)
                 GROUP BY task_type,monday ORDER BY task_type,monday LIMIT ?3",
            )
            .map_err(sql)?;
        let rows = query
            .query_map(params![since, task_type, MAX_METRICS_ROWS as i64], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<f64>>(4)?,
                    r.get::<_, Option<f64>>(5)?,
                    r.get::<_, Option<f64>>(6)?,
                ))
            })
            .map_err(sql)?;
        rows.map(|row| {
            let (task_type, monday, requests, verified, calls, cost, cached) = row.map_err(sql)?;
            let (year, week) = iso_week_of_monday(monday);
            let (start_year, month, day) = civil_from_days(monday);
            let requests = requests.max(0) as u64;
            let verified = verified.max(0) as u64;
            Ok(WeeklyMetrics {
                task_type,
                week: format!("{year}-W{week:02}"),
                week_start: format!("{start_year:04}-{month:02}-{day:02}"),
                requests,
                verified,
                verified_rate: if requests == 0 {
                    0.0
                } else {
                    verified as f64 / requests as f64
                },
                mean_model_calls: calls.unwrap_or(0.0),
                mean_cost_usd: cost.unwrap_or(0.0),
                mean_cached_tokens: cached.unwrap_or(0.0),
            })
        })
        .collect()
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
