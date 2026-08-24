use super::{format_per_file_staleness_banner, humanize_age, needs_lazy_sync_before_dispatch};
use std::fs;
use tempfile::tempdir;

#[test]
fn humanize_age_picks_right_unit() {
    assert_eq!(humanize_age(0), "0s ago");
    assert_eq!(humanize_age(45), "45s ago");
    assert_eq!(humanize_age(125), "2m ago");
    assert_eq!(humanize_age(3_700), "1h ago");
    assert_eq!(humanize_age(90_000), "1d ago");
}

#[test]
fn banner_lists_stale_files_with_age() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "fn a() {}").unwrap();
    fs::write(root.join("src/b.rs"), "fn b() {}").unwrap();

    let stale = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    let banner = format_per_file_staleness_banner(root, &stale);
    assert!(banner.contains("2 file(s) referenced below were edited"));
    assert!(banner.contains("src/"));
    assert!(banner.contains("a.rs ("));
    assert!(banner.contains("b.rs ("));
    assert!(banner.contains("ago)"));
    assert!(banner.contains("tracedecay sync"));
    // Critical UX shift: should NOT say "STALE INDEX" — the whole
    // point of #428 is to scope the warning, not blanket-distrust
    // the entire response.
    assert!(!banner.contains("STALE INDEX"));
}

#[test]
fn banner_handles_missing_file_gracefully() {
    let tmp = tempdir().unwrap();
    let stale = vec!["does/not/exist.rs".to_string()];
    let banner = format_per_file_staleness_banner(tmp.path(), &stale);
    // Missing files still get listed (e.g. file deleted between
    // sync and tool response). Age falls back to 0s.
    assert!(banner.contains("does/not/exist.rs"));
}

#[test]
fn read_only_tools_skip_lazy_sync_before_dispatch() {
    for tool in [
        "tracedecay_active_project",
        "tracedecay_context",
        "tracedecay_files",
        "tracedecay_runtime",
        "tracedecay_search",
        "tracedecay_status",
        "tracedecay_storage_status",
    ] {
        assert!(
            !needs_lazy_sync_before_dispatch(tool),
            "{tool} should stay available when lazy sync is stuck"
        );
    }

    for tool in [
        "tracedecay_insert_at",
        "tracedecay_multi_str_replace",
        "tracedecay_replace_symbol",
        "tracedecay_str_replace",
    ] {
        assert!(
            needs_lazy_sync_before_dispatch(tool),
            "{tool} should still get the normal lazy freshness check"
        );
    }
}
