//! Retiring lessons that underperform (design section 15 "Retiring lessons";
//! Requirement 17.4; Property 15, retirement part; task 25.2).
//!
//! A lesson's **shown uses** are its `learn_lesson_uses` rows whose `verified` was decided
//! when the request closed ([`SqliteLearning::decide_lesson_uses`]). Its **rate** is the
//! share of those uses whose request closed `verified`.
//!
//! The **comparison group** is the closed requests (`learn_runs`) of the lesson's task type
//! that ended in the last [`COMPARISON_WINDOW_MS`] (90 days) and did not show the lesson.
//! The task type compares with `IS`, so a lesson without a task type is compared with
//! requests without one. Requests of every agent count, as the design states. Its rate is
//! the share of those requests with status `verified`.
//!
//! [`should_retire`] decides: at least [`MIN_SHOWN_USES`] shown uses, at least
//! [`MIN_COMPARISON_REQUESTS`] comparison requests, and a lesson rate strictly below the
//! comparison rate. The Host checks, at each request close and after the uses are decided,
//! the lessons that request showed ([`SqliteLearning::retire_underperforming`]). Both
//! counts are indexed lookups: `learn_lesson_uses` by its `(lesson_id, request_id)` key
//! and `learn_runs` by `(task_type, ended)`.
//!
//! Only an `active` lesson is retired; it gets status `retired` and `retired_at`. Nothing
//! returns a retired lesson to `active`: merging keeps the status (see [`crate::lessons`])
//! and the learned context reads only active lessons (see [`crate::context`]).
use crate::store::{now_ms, sql, SqliteLearning};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::io;

/// Decided shown uses a lesson needs before it can be retired.
pub const MIN_SHOWN_USES: u64 = 10;
/// Comparison requests needed before a lesson can be retired.
pub const MIN_COMPARISON_REQUESTS: u64 = 10;
/// How far back comparison requests are read (90 days).
pub const COMPARISON_WINDOW_MS: u64 = 90 * 24 * 3600 * 1000;

/// Whether a lesson with `lesson_verified` verified requests out of `shown_uses` decided
/// uses is retired against a comparison group of `comparison_total` requests of which
/// `comparison_verified` were verified. Both groups need their minimum size, and the
/// lesson's rate must be strictly lower. The rates compare exactly (cross-multiplied).
/// Counts above their totals are clamped to the totals.
pub fn should_retire(
    shown_uses: u64,
    lesson_verified: u64,
    comparison_total: u64,
    comparison_verified: u64,
) -> bool {
    if shown_uses < MIN_SHOWN_USES || comparison_total < MIN_COMPARISON_REQUESTS {
        return false;
    }
    let lesson = u128::from(lesson_verified.min(shown_uses));
    let comparison = u128::from(comparison_verified.min(comparison_total));
    // lesson / shown < comparison / total
    lesson * u128::from(comparison_total) < comparison * u128::from(shown_uses)
}

/// The counts one retirement decision used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetirementCounts {
    pub shown_uses: u64,
    pub lesson_verified: u64,
    pub comparison_total: u64,
    pub comparison_verified: u64,
}

impl SqliteLearning {
    /// Retires the active lessons shown in `request` that underperform, at the current
    /// time. Returns the retired lesson ids, ascending. See the module documentation.
    pub fn retire_underperforming(&self, request: &str) -> io::Result<Vec<i64>> {
        self.retire_underperforming_at(request, now_ms())
    }

    /// [`Self::retire_underperforming`] at time `now` (Unix ms).
    pub fn retire_underperforming_at(&self, request: &str, now: u64) -> io::Result<Vec<i64>> {
        let since = i64::try_from(now.saturating_sub(COMPARISON_WINDOW_MS)).unwrap_or(i64::MAX);
        let retired_at = i64::try_from(now).unwrap_or(i64::MAX);
        let mut db = self.lock()?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let lessons: Vec<(i64, Option<String>)> = {
            let mut query = tx
                .prepare(
                    "SELECT l.id,l.task_type FROM learn_lesson_uses u JOIN learn_lessons l ON l.id=u.lesson_id
                     WHERE u.request_id=? AND l.status='active' ORDER BY l.id",
                )
                .map_err(sql)?;
            let rows = query
                .query_map([request], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(sql)?;
            rows.collect::<rusqlite::Result<_>>().map_err(sql)?
        };
        let mut retired = Vec::new();
        for (id, task_type) in lessons {
            let counts = counts(&tx, id, task_type.as_deref(), since)?;
            if should_retire(
                counts.shown_uses,
                counts.lesson_verified,
                counts.comparison_total,
                counts.comparison_verified,
            ) {
                let changed = tx
                    .execute(
                        "UPDATE learn_lessons SET status='retired',retired_at=? WHERE id=? AND status='active'",
                        params![retired_at, id],
                    )
                    .map_err(sql)?;
                if changed == 1 {
                    retired.push(id);
                }
            }
        }
        tx.commit().map_err(sql)?;
        Ok(retired)
    }

    /// The counts [`should_retire`] gets for `lesson` at time `now`, or `None` for an
    /// unknown lesson.
    pub fn retirement_counts(&self, lesson: i64, now: u64) -> io::Result<Option<RetirementCounts>> {
        let since = i64::try_from(now.saturating_sub(COMPARISON_WINDOW_MS)).unwrap_or(i64::MAX);
        let db = self.lock()?;
        let task_type: Option<Option<String>> = db
            .query_row(
                "SELECT task_type FROM learn_lessons WHERE id=?",
                [lesson],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        task_type
            .map(|task_type| counts(&db, lesson, task_type.as_deref(), since))
            .transpose()
    }
}

fn counts(
    db: &rusqlite::Connection,
    lesson: i64,
    task_type: Option<&str>,
    since: i64,
) -> io::Result<RetirementCounts> {
    let (shown, verified): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(verified<>0),0) FROM learn_lesson_uses
             WHERE lesson_id=? AND verified IS NOT NULL",
            [lesson],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql)?;
    let (total, comparison_verified): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(r.status='verified'),0) FROM learn_runs r
             WHERE r.task_type IS ?1 AND r.ended>=?2
             AND NOT EXISTS(SELECT 1 FROM learn_lesson_uses u WHERE u.lesson_id=?3 AND u.request_id=r.request_id)",
            params![task_type, since, lesson],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql)?;
    Ok(RetirementCounts {
        shown_uses: shown.max(0) as u64,
        lesson_verified: verified.max(0) as u64,
        comparison_total: total.max(0) as u64,
        comparison_verified: comparison_verified.max(0) as u64,
    })
}

#[cfg(test)]
#[path = "retirement_tests.rs"]
mod tests;
