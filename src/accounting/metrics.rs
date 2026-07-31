//! Aggregation and summary queries for token accounting.

use crate::global_db::RegisteredGlobalDb;

/// Full cost summary with breakdowns.
pub struct CostSummary {
    pub total_cost: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub by_model: Vec<(String, f64, u64)>, // (model, cost, total_tokens)
    pub by_category: Vec<(String, f64, u64)>, // (category, cost, turn_count)
    pub tokens_saved: u64,
    pub efficiency_ratio: f64,
}

/// Build a full cost summary for a given time range.
pub(crate) async fn cost_summary(
    gdb: &RegisteredGlobalDb,
    since: u64,
    tokens_saved: u64,
) -> Result<CostSummary, String> {
    let total_cost = gdb.try_total_cost_since(since).await?;
    let (total_input, total_output, total_cache_read) =
        gdb.try_token_breakdown_since(since).await?;
    let by_model = gdb.try_cost_by_model_since(since).await?;
    let by_category = gdb.try_cost_by_category_since(since).await?;

    let total_consumed = total_input + total_output;
    let efficiency_ratio = if tokens_saved + total_consumed > 0 {
        tokens_saved as f64 / (tokens_saved + total_consumed) as f64
    } else {
        0.0
    };

    Ok(CostSummary {
        total_cost,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cache_read_tokens: total_cache_read,
        by_model,
        by_category,
        tokens_saved,
        efficiency_ratio,
    })
}

/// Parse a range string into a unix timestamp for "since".
pub fn parse_range(range: &str) -> u64 {
    let now = now_epoch();
    match range {
        "today" => today_start_epoch(now),
        "30d" => now.saturating_sub(30 * 86400),
        "month" => month_start_epoch(now),
        "all" => 0,
        _ => now.saturating_sub(7 * 86400),
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Start of today (midnight UTC).
fn today_start_epoch(now: u64) -> u64 {
    now - (now % 86400)
}

/// Start of the current calendar month (UTC).
/// Uses 30 days as an approximation to avoid pulling in chrono.
fn month_start_epoch(now: u64) -> u64 {
    now.saturating_sub(30 * 86400)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range() {
        let now = now_epoch();
        let today = parse_range("today");
        assert!(today <= now);
        assert!(now - today < 86400);

        let week = parse_range("7d");
        assert!(now - week >= 7 * 86400 - 1);
        assert!(now - week <= 7 * 86400 + 1);

        assert_eq!(parse_range("all"), 0);
    }

    #[test]
    fn test_today_start() {
        // Use a value that's exactly at midnight UTC (divisible by 86400)
        let midnight = (1_713_100_800 / 86400) * 86400;
        assert_eq!(today_start_epoch(midnight), midnight);
        assert_eq!(today_start_epoch(midnight + 3600), midnight);
        assert_eq!(today_start_epoch(midnight + 86399), midnight);
    }
}
