use anyhow::Result;

/// Merge strategy: last-write-wins based on `updated_at`.
/// The actual SQL UPSERT logic lives in infra/db.rs;
/// this module documents the policy and provides helper comparators.

pub struct MergePolicy;

impl MergePolicy {
    /// Returns true if `remote_ts` is newer and should win over `local_ts`.
    pub fn remote_wins(local_ts: i64, remote_ts: i64) -> bool {
        remote_ts > local_ts
    }

    /// Validate that a sync timestamp is sane (not negative, not future skew > 1 day).
    pub fn is_valid_timestamp(ts: i64) -> bool {
        let now = chrono::Utc::now().timestamp();
        ts >= 0 && ts <= now + 86_400
    }
}

/// Represents the result of a merge operation.
#[derive(Debug, Default)]
pub struct MergeReport {
    pub upserted: usize,
    pub skipped: usize,
}

impl MergeReport {
    pub fn record_upsert(&mut self) {
        self.upserted += 1;
    }

    pub fn record_skip(&mut self) {
        self.skipped += 1;
    }
}

/// Placeholder: validate sync result is usable.
pub fn validate_sync(report: &MergeReport) -> Result<()> {
    log::info!(
        "Sync complete: {} upserted, {} skipped",
        report.upserted,
        report.skipped
    );
    Ok(())
}
