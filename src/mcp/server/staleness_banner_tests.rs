use super::needs_lazy_sync_before_dispatch;

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
