use std::collections::BTreeMap;

use serde_json::Value;

use crate::global_db::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
};

pub(super) const MAX_DIAGNOSIS_FINDINGS: usize = 64;
const MAX_FINDING_COUNT: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TemporalHealthLineLevel {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TemporalHealthLine {
    pub(super) level: TemporalHealthLineLevel,
    pub(super) text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TemporalHealthDiagnosis {
    #[cfg(test)]
    clean: bool,
    #[cfg(test)]
    findings: Vec<SessionTemporalHealthFindingKind>,
    lines: Vec<TemporalHealthLine>,
}

impl TemporalHealthDiagnosis {
    #[cfg(test)]
    pub(super) const fn is_clean(&self) -> bool {
        self.clean
    }

    #[cfg(test)]
    pub(super) fn finding_codes(&self) -> Vec<&'static str> {
        self.findings
            .iter()
            .map(|finding| finding_contract(*finding).0)
            .collect()
    }

    pub(super) fn lines(&self) -> &[TemporalHealthLine] {
        &self.lines
    }
}

#[cfg(test)]
pub(super) fn diagnose(payload: Option<&Value>) -> TemporalHealthDiagnosis {
    diagnose_with_recovery(payload, false)
}

pub(super) fn diagnose_with_recovery(
    payload: Option<&Value>,
    recovery_pending: bool,
) -> TemporalHealthDiagnosis {
    let Some(payload) = payload else {
        return availability_diagnosis(SessionTemporalHealthStatus::Unavailable);
    };
    let Ok(report) = serde_json::from_value::<SessionTemporalHealthReport>(payload.clone()) else {
        return availability_diagnosis(SessionTemporalHealthStatus::Unavailable);
    };

    let bounded = report.findings().len() > MAX_DIAGNOSIS_FINDINGS;
    let status = if bounded {
        SessionTemporalHealthStatus::Partial
    } else {
        report.status()
    };
    let mut counts = BTreeMap::new();
    for finding in report.findings().iter().take(MAX_DIAGNOSIS_FINDINGS) {
        if finding.count() == 0 {
            continue;
        }
        counts
            .entry(finding.kind())
            .and_modify(|count: &mut u64| {
                *count = count.saturating_add(finding.count()).min(MAX_FINDING_COUNT);
            })
            .or_insert(finding.count().min(MAX_FINDING_COUNT));
    }

    let mut lines = availability_lines(status);
    let clean = status == SessionTemporalHealthStatus::Complete && counts.is_empty();
    #[cfg(test)]
    let findings = counts.keys().copied().collect::<Vec<_>>();
    for (kind, count) in counts {
        let (code, label, action) = finding_contract(kind);
        lines.push(TemporalHealthLine {
            level: if recovery_pending
                && kind == SessionTemporalHealthFindingKind::CursorKeyAbsent
            {
                TemporalHealthLineLevel::Warn
            } else {
                TemporalHealthLineLevel::Fail
            },
            text: format!(
                "Temporal health [{code}] {label}: {count} violation(s). Recovery is daemon-owned. {action} Doctor is read-only and performed no repair."
            ),
        });
    }
    if clean {
        lines.push(TemporalHealthLine {
            level: TemporalHealthLineLevel::Pass,
            text: "Session temporal health: complete and clean (read-only daemon audit)"
                .to_string(),
        });
    }
    TemporalHealthDiagnosis {
        #[cfg(test)]
        clean,
        #[cfg(test)]
        findings,
        lines,
    }
}

fn availability_diagnosis(status: SessionTemporalHealthStatus) -> TemporalHealthDiagnosis {
    TemporalHealthDiagnosis {
        #[cfg(test)]
        clean: false,
        #[cfg(test)]
        findings: Vec::new(),
        lines: availability_lines(status),
    }
}

fn availability_lines(status: SessionTemporalHealthStatus) -> Vec<TemporalHealthLine> {
    let state = match status {
        SessionTemporalHealthStatus::Complete => return Vec::new(),
        SessionTemporalHealthStatus::Partial => "partial",
        SessionTemporalHealthStatus::Unavailable => "unavailable",
        SessionTemporalHealthStatus::Locked => "locked",
    };
    vec![TemporalHealthLine {
        level: TemporalHealthLineLevel::Warn,
        text: format!(
            "Session temporal health diagnosis is {state}; no clean result was inferred. Preserve the database and retry the daemon-owned health operation after active writers settle. Doctor is read-only and performed no repair."
        ),
    }]
}

fn finding_contract(
    kind: SessionTemporalHealthFindingKind,
) -> (&'static str, &'static str, &'static str) {
    match kind {
        SessionTemporalHealthFindingKind::TriggerAuditDrift => (
            "trigger_audit_drift",
            "trigger audit contract drift",
            "Preserve the database, upgrade or restart the daemon owner, then rerun Doctor; do not edit temporal triggers manually.",
        ),
        SessionTemporalHealthFindingKind::OccurrenceFtsCorruption => (
            "occurrence_fts_corruption",
            "occurrence derived-index corruption",
            "Preserve the database and request explicit derived-index repair from the daemon-owned writer; only session_occurrences_fts is repairable.",
        ),
        SessionTemporalHealthFindingKind::SummaryFtsCorruption => (
            "summary_fts_corruption",
            "summary derived-index corruption",
            "Preserve the database and request explicit derived-index repair from the daemon-owned writer; only session_summary_nodes_fts is repairable.",
        ),
        SessionTemporalHealthFindingKind::SummaryCycle => (
            "summary_cycle",
            "summary lineage cycle",
            "Pause temporal refresh, preserve the database, and report this stable code for daemon-owned lineage recovery.",
        ),
        SessionTemporalHealthFindingKind::StaleClosure => (
            "stale_closure",
            "incomplete summary stale closure",
            "Pause temporal refresh and rerun the daemon-owned stale-closure rebuild before serving summaries.",
        ),
        SessionTemporalHealthFindingKind::MissingAnchor => (
            "missing_anchor",
            "missing retrieval anchor",
            "Pause temporal refresh, preserve the database, and request daemon-owned projection recovery.",
        ),
        SessionTemporalHealthFindingKind::MissingReceipt => (
            "missing_receipt",
            "missing authority receipt",
            "Pause temporal refresh and preserve the database; authority receipts require daemon-owned replay, not row synthesis.",
        ),
        SessionTemporalHealthFindingKind::InvalidGeneration => (
            "invalid_generation",
            "invalid generation state",
            "Pause temporal refresh and preserve the database before daemon-owned generation recovery.",
        ),
        SessionTemporalHealthFindingKind::MultiActiveGeneration => (
            "multi_active_generation",
            "multiple active generations for one session",
            "Pause temporal refresh immediately and preserve the database; the daemon owner must reconcile generation activation.",
        ),
        SessionTemporalHealthFindingKind::CursorChainAbsent => (
            "cursor_chain_absent",
            "cursor key version chain gap",
            "Pause temporal refresh and rotate or recover cursor keys through the daemon owner; never recreate key rows manually.",
        ),
        SessionTemporalHealthFindingKind::CursorKeyAbsent => (
            "cursor_key_absent",
            "active generation cursor key absence",
            "Pause temporal refresh and recover the referenced active cursor key through the daemon owner.",
        ),
        SessionTemporalHealthFindingKind::OwnershipDrift => (
            "ownership_drift",
            "cross-session or cross-generation ownership drift",
            "Pause temporal refresh, preserve the database, and request daemon-owned ownership reconciliation.",
        ),
        SessionTemporalHealthFindingKind::StuckRefresh => (
            "stuck_refresh",
            "stale running refresh",
            "Pause temporal refresh and let the daemon owner resume or cancel the stale operation.",
        ),
        SessionTemporalHealthFindingKind::StuckBinding => (
            "stuck_binding",
            "refresh binding drift",
            "Pause temporal refresh and request daemon-owned binding recovery before retrying.",
        ),
        SessionTemporalHealthFindingKind::StuckProgress => (
            "stuck_progress",
            "refresh progress stall",
            "Pause temporal refresh and request daemon-owned resume from the last durable progress receipt.",
        ),
        SessionTemporalHealthFindingKind::StuckReceipt => (
            "stuck_receipt",
            "refresh terminal receipt drift",
            "Pause temporal refresh and request daemon-owned terminal receipt reconciliation.",
        ),
        SessionTemporalHealthFindingKind::MigrationGap => (
            "migration_gap",
            "temporal schema migration gap",
            "Preserve the database, upgrade the daemon owner, and rerun Doctor; do not initialize or force-sync the store.",
        ),
        SessionTemporalHealthFindingKind::CompatibilityDrift => (
            "compatibility_drift",
            "canonical-to-compatibility projection drift",
            "Preserve canonical temporal rows and request a daemon-owned compatibility projection rebuild.",
        ),
    }
}
