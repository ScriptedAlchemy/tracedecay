use tracedecay_sessions::runtime::lcm::LcmGcReport;
use tracedecay_sessions::runtime::lcm::gc::{
    LcmGcDeferredReport, LcmGcPhaseReport, LcmGcReportConfig, LcmGcTotals,
};

#[test]
fn lcm_gc_report_remains_struct_literal_compatible() {
    let report = LcmGcReport {
        status: "dry_run".to_string(),
        provider: "all".to_string(),
        session_id: None,
        apply: false,
        started_at: 0,
        ended_at: 0,
        config: LcmGcReportConfig {
            grace_seconds: 0,
            reap_missing_after: 0,
            max_batch_size: 1,
        },
        orphans: LcmGcPhaseReport::default(),
        unreferenced: LcmGcPhaseReport::default(),
        missing: LcmGcPhaseReport::default(),
        dangling: LcmGcPhaseReport::default(),
        deferred: LcmGcDeferredReport::default(),
        errors: Vec::new(),
        totals: LcmGcTotals::default(),
        last_gc_at: None,
        last_error: None,
        backup: None,
    };

    assert_eq!(report.status, "dry_run");
}
