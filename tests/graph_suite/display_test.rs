use std::time::{SystemTime, UNIX_EPOCH};
use tracedecay::display::{
    StatusTable, format_bytes, format_number, format_relative_time, format_token_count,
    print_status_header, print_status_table_with,
};

// ── format_token_count ──────────────────────────────────────────────────────

#[test]
fn test_format_token_count_zero() {
    assert_eq!(format_token_count(0), "0");
}

#[test]
fn test_format_token_count_small() {
    assert_eq!(format_token_count(42), "42");
    assert_eq!(format_token_count(999), "999");
}

#[test]
fn test_format_token_count_thousands() {
    assert_eq!(format_token_count(1_000), "1.0k");
    assert_eq!(format_token_count(1_500), "1.5k");
    assert_eq!(format_token_count(45_300), "45.3k");
    assert_eq!(format_token_count(999_999), "1000.0k");
}

#[test]
fn test_format_token_count_millions() {
    assert_eq!(format_token_count(1_000_000), "1.0M");
    assert_eq!(format_token_count(1_200_000), "1.2M");
    assert_eq!(format_token_count(123_456_789), "123.5M");
}

// ── format_bytes ────────────────────────────────────────────────────────────

#[test]
fn test_format_bytes_small() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(1023), "1023 B");
}

#[test]
fn test_format_bytes_kilobytes() {
    assert_eq!(format_bytes(1024), "1.0 KB");
    assert_eq!(format_bytes(1_536), "1.5 KB");
    assert_eq!(format_bytes(1_048_575), "1024.0 KB");
}

#[test]
fn test_format_bytes_megabytes() {
    assert_eq!(format_bytes(1_048_576), "1.0 MB");
    assert_eq!(format_bytes(838_860_800), "800.0 MB");
}

#[test]
fn test_format_bytes_gigabytes() {
    assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    assert_eq!(format_bytes(2_684_354_560), "2.5 GB");
}

// ── format_number ───────────────────────────────────────────────────────────

#[test]
fn test_format_number_no_commas() {
    assert_eq!(format_number(0), "0");
    assert_eq!(format_number(1), "1");
    assert_eq!(format_number(999), "999");
}

#[test]
fn test_format_number_with_commas() {
    assert_eq!(format_number(1_000), "1,000");
    assert_eq!(format_number(12_345), "12,345");
    assert_eq!(format_number(243_302), "243,302");
    assert_eq!(format_number(1_000_000), "1,000,000");
    assert_eq!(format_number(1_234_567_890), "1,234,567,890");
}

// ── format_relative_time ────────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn test_format_relative_time_never() {
    assert_eq!(format_relative_time(0), "never");
}

#[test]
fn test_format_relative_time_seconds_ago() {
    let ts = now_secs() - 30;
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("s ago"),
        "expected '...s ago', got '{result}'"
    );
}

#[test]
fn test_format_relative_time_minutes_ago() {
    let ts = now_secs() - 300; // 5 minutes
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("m ago"),
        "expected '...m ago', got '{result}'"
    );
}

#[test]
fn test_format_relative_time_hours_ago() {
    let ts = now_secs() - 7200; // 2 hours
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("h ago"),
        "expected '...h ago', got '{result}'"
    );
}

#[test]
fn test_format_relative_time_days_ago() {
    let ts = now_secs() - 172_800; // 2 days
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("d ago"),
        "expected '...d ago', got '{result}'"
    );
}

#[test]
fn test_format_relative_time_boundary_59s() {
    let ts = now_secs() - 59;
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("s ago"),
        "59s should still be seconds, got '{result}'"
    );
}

#[test]
fn test_format_relative_time_boundary_60s() {
    // 60 seconds = 1 minute
    let ts = now_secs() - 60;
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("m ago"),
        "60s should be minutes, got '{result}'"
    );
}

#[test]
fn test_format_relative_time_boundary_3599s() {
    let ts = now_secs() - 3599;
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("m ago"),
        "3599s should be minutes, got '{result}'"
    );
}

#[test]
fn test_format_relative_time_boundary_3600s() {
    let ts = now_secs() - 3600;
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("h ago"),
        "3600s should be hours, got '{result}'"
    );
}

#[test]
fn test_format_relative_time_boundary_86399s() {
    let ts = now_secs() - 86399;
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("h ago"),
        "86399s should be hours, got '{result}'"
    );
}

#[test]
fn test_format_relative_time_boundary_86400s() {
    let ts = now_secs() - 86400;
    let result = format_relative_time(ts);
    assert!(
        result.ends_with("d ago"),
        "86400s should be days, got '{result}'"
    );
}

#[test]
fn test_format_relative_time_future_timestamp() {
    // Timestamp in the future — saturating_sub should yield 0 delta → "0s ago"
    let ts = now_secs() + 1000;
    let result = format_relative_time(ts);
    assert_eq!(result, "0s ago");
}

// ── helpers for status table tests ──────────────────────────────────────────

use tracedecay::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1;
use tracedecay::runtime_telemetry::{GenerationCensusSnapshot, GenerationCensusUnavailableReason};
use tracedecay_code_index::production::CodeIndexGenerationStatisticsV1;

fn observed_census() -> GenerationCensusSnapshot {
    GenerationCensusSnapshot::Observed {
        statistics: CodeIndexGenerationStatisticsV1 {
            source_total_bytes: 2_000_000,
            symbol_count: 170,
            edge_count: 300,
        },
    }
}

fn unavailable_census() -> GenerationCensusSnapshot {
    GenerationCensusSnapshot::Unavailable {
        reason: GenerationCensusUnavailableReason::ExactScopeGenerationNotReady,
    }
}

fn sample_freshness() -> CodeIndexWorktreeFreshnessV1 {
    CodeIndexWorktreeFreshnessV1 {
        worktree_root: "/tmp/project".to_owned(),
        repository_id: None,
        worktree_id: None,
        source_reference: None,
        source_revision: None,
        latest_generation_id: Some("generation-1".to_owned()),
        snapshot_content_identity: None,
        sealed_at_micros: Some(1_700_000_000_000_000),
        last_reconcile_micros: Some(1_700_000_100_000_000),
        staleness_state: Some("fresh".to_owned()),
        hook_hint_count: None,
        coverage: "complete".to_owned(),
    }
}

// ── print_status_table_with ─────────────────────────────────────────────────

fn status_table<'a>(census: &'a GenerationCensusSnapshot) -> StatusTable<'a> {
    StatusTable {
        census,
        freshness: None,
        tokens_saved: 0,
        global_tokens_saved: None,
        worldwide: None,
        country_flags: &[],
        branch_info: None,
        cost_info: None,
    }
}

#[test]
fn test_print_status_table_no_flags_no_worldwide() {
    let census = observed_census();
    print_status_table_with(StatusTable {
        tokens_saved: 50_000,
        ..status_table(&census)
    });
}

#[test]
fn test_print_status_table_with_flags() {
    let census = observed_census();
    let flags = vec![
        "\u{1f1fa}\u{1f1f8}".to_string(),
        "\u{1f1ec}\u{1f1e7}".to_string(),
    ];
    print_status_table_with(StatusTable {
        tokens_saved: 50_000,
        country_flags: &flags,
        ..status_table(&census)
    });
}

#[test]
fn test_print_status_table_with_worldwide() {
    let census = observed_census();
    print_status_table_with(StatusTable {
        tokens_saved: 50_000,
        worldwide: Some(10_000_000),
        ..status_table(&census)
    });
}

#[test]
fn test_print_status_table_with_global_tokens() {
    let census = observed_census();
    print_status_table_with(StatusTable {
        tokens_saved: 50_000,
        global_tokens_saved: Some(200_000),
        ..status_table(&census)
    });
}

#[test]
fn test_print_status_table_with_all_options() {
    let census = observed_census();
    let freshness = sample_freshness();
    let flags = vec![
        "\u{1f1fa}\u{1f1f8}".to_string(),
        "\u{1f1e9}\u{1f1ea}".to_string(),
        "\u{1f1ef}\u{1f1f5}".to_string(),
    ];
    print_status_table_with(StatusTable {
        freshness: Some(&freshness),
        tokens_saved: 100_000,
        global_tokens_saved: Some(500_000),
        worldwide: Some(50_000_000),
        country_flags: &flags,
        ..status_table(&census)
    });
}

#[test]
fn test_print_status_table_unavailable_census_prints_typed_reason() {
    let census = unavailable_census();
    print_status_table_with(status_table(&census));
}

#[test]
fn test_print_status_table_large_token_values() {
    let census = observed_census();
    print_status_table_with(StatusTable {
        tokens_saved: 999_999_999,
        global_tokens_saved: Some(1_000_000_000),
        worldwide: Some(50_000_000_000),
        ..status_table(&census)
    });
}

#[test]
fn test_print_status_table_many_flags() {
    let census = observed_census();
    // 30 flags — exceeds MAX_DISPLAY_FLAGS (25), should trigger truncation.
    let flags: Vec<String> = (0..30).map(|_| "\u{1f1fa}\u{1f1f8}".to_string()).collect();
    print_status_table_with(StatusTable {
        tokens_saved: 50_000,
        country_flags: &flags,
        ..status_table(&census)
    });
}

// ── print_status_header ─────────────────────────────────────────────────────

#[test]
fn test_print_status_header_no_flags_no_worldwide() {
    let census = observed_census();
    print_status_header(&census, None, 50_000, None, None, &[], None, None);
}

#[test]
fn test_print_status_header_with_freshness_and_flags() {
    let census = observed_census();
    let freshness = sample_freshness();
    let flags = vec![
        "\u{1f1fa}\u{1f1f8}".to_string(),
        "\u{1f1ec}\u{1f1e7}".to_string(),
    ];
    print_status_header(
        &census,
        Some(&freshness),
        50_000,
        None,
        None,
        &flags,
        None,
        None,
    );
}

#[test]
fn test_print_status_header_with_all_options() {
    let census = observed_census();
    let flags = vec![
        "\u{1f1fa}\u{1f1f8}".to_string(),
        "\u{1f1e9}\u{1f1ea}".to_string(),
        "\u{1f1ef}\u{1f1f5}".to_string(),
    ];
    print_status_header(
        &census,
        None,
        100_000,
        Some(500_000),
        Some(50_000_000),
        &flags,
        None,
        None,
    );
}

#[test]
fn test_print_status_header_unavailable_census() {
    let census = unavailable_census();
    print_status_header(&census, None, 0, None, None, &[], None, None);
}
