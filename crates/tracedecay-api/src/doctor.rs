//! Read-only Doctor/health route descriptors and DTO mapping.
//!
//! This module owns the HTTP presentation of the canonical Doctor report: the
//! closed finding-family vocabulary, the query DTO, the per-route descriptors,
//! and the projection from an admitted [`DoctorReportV1`] onto the
//! [`crate::read_model`] envelope axes (coverage, freshness, and domain state).
//!
//! It evaluates no health and offers no mutation path. The executable hands
//! this module an admitted report and receives presentation, never a verdict.

use std::fmt;

use serde::Deserialize;
use tracedecay_application::doctor::{
    DoctorCoverageCompletenessV1, DoctorEvidenceStateV1, DoctorFamilyConsultationV1,
    DoctorFamilyUnavailableReasonV1, DoctorFindingFamilyV1, DoctorReportCoverageV1,
    DoctorReportEntryV1, DoctorReportV1,
};

use crate::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardFreshnessStateV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, DashboardLegalActionRefV1,
};

/// Owning application operation for a Doctor finding re-read.
pub const DOCTOR_FINDINGS_REFRESH_OPERATION: &str = "use-case.dashboard.doctor.findings.refresh";

/// Note for a dashboard scope that was opened without an admitted Doctor report
/// source. The absence is typed unsupported, never a clean report.
pub const DOCTOR_REPORT_SOURCE_UNSUPPORTED_NOTE: &str =
    "no admitted Doctor report source is available for this dashboard scope";

/// The closed Doctor finding-family vocabulary the read routes project.
pub use tracedecay_application::doctor::DOCTOR_FINDING_FAMILIES as KNOWN_DOCTOR_FINDING_FAMILIES;

/// Path of the Doctor finding read route, filtered by the caller's query.
pub const DOCTOR_FINDINGS_ROUTE_PATH: &str = "/api/doctor/findings";

/// Path of the storage-family compatibility projection of the same report.
pub const STORAGE_FINDINGS_ROUTE_PATH: &str = "/api/storage/findings";

/// Query DTO for the Doctor findings read route.
///
/// Unknown query parameters are ignored rather than rejected; only `family` is
/// interpreted, and it is validated against the closed vocabulary by
/// [`parse_doctor_finding_family`].
#[derive(Clone, Debug, Default, Deserialize)]
pub struct DoctorFindingsQueryV1 {
    /// Optional per-family filter (`advisory`, `configuration`,
    /// `storage_runtime`, `storage`, `language_server`, `semantic_index`,
    /// `observability`).
    #[serde(default)]
    pub family: Option<String>,
}

/// Parse a `snake_case` family label against the closed vocabulary. `Ok(None)`
/// means no filter was supplied; `Err` carries the invalid label.
pub fn parse_doctor_finding_family(
    family: Option<&str>,
) -> Result<Option<DoctorFindingFamilyV1>, String> {
    let Some(raw) = family else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let quoted = format!("\"{trimmed}\"");
    serde_json::from_str::<DoctorFindingFamilyV1>(&quoted)
        .map(Some)
        .map_err(|_| trimmed.to_string())
}

/// The stable label for one finding family, used in coverage omission reasons.
pub use tracedecay_application::doctor::doctor_finding_family_label;

/// The refresh action every Doctor read attaches, including its typed
/// unavailable states: a caller can always re-read.
#[must_use]
pub fn doctor_findings_refresh_action() -> DashboardLegalActionRefV1 {
    DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        DOCTOR_FINDINGS_REFRESH_OPERATION,
    )
}

/// Note for an admitted report source that failed to compose a report.
#[must_use]
pub fn doctor_report_failure_note(error: &dyn fmt::Display) -> String {
    format!("Doctor report composition failed: {error}")
}

/// Envelope-level presentation for one Doctor/health read.
///
/// This carries every axis the read model judges. Route payloads are assembled
/// by their owning surface, which may attach owner-supplied dispatch targets
/// this adapter cannot construct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoctorReadPresentationV1 {
    pub domain_state: DashboardDomainStateV1,
    pub coverage: DashboardCoverageV1,
    pub freshness: DashboardFreshnessV1,
    pub legal_actions: Vec<DashboardLegalActionRefV1>,
}

impl DoctorReadPresentationV1 {
    /// No admitted report source exists for this scope. Coverage and freshness
    /// are typed unsupported so absence never reads as a healthy empty report.
    #[must_use]
    pub fn source_unsupported() -> Self {
        Self {
            domain_state: DashboardDomainStateV1::Unsupported,
            coverage: DashboardCoverageV1::unsupported(),
            freshness: DashboardFreshnessV1::unsupported(),
            legal_actions: vec![doctor_findings_refresh_action()],
        }
    }

    /// The admitted source exists but this observation failed or was rejected.
    #[must_use]
    pub fn source_failed() -> Self {
        Self {
            domain_state: DashboardDomainStateV1::Error,
            coverage: DashboardCoverageV1::unknown(),
            freshness: DashboardFreshnessV1::unknown(),
            legal_actions: vec![doctor_findings_refresh_action()],
        }
    }
}

/// Why a Doctor report could not be projected for a route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoctorProjectionErrorV1 {
    /// The canonical report carried no entry for the requested family. An
    /// absent family is not a clean family.
    FamilyAbsent,
}

impl DoctorProjectionErrorV1 {
    /// The note a route surfaces for this rejection.
    #[must_use]
    pub fn note(&self) -> String {
        "canonical Doctor report omitted the requested finding family".to_owned()
    }
}

impl fmt::Display for DoctorProjectionErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.note())
    }
}

/// A projected Doctor report for one read route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoctorFindingsProjectionV1 {
    /// The canonical entries that survive the family filter, subclass intact.
    pub entries: Vec<DoctorReportEntryV1>,
    /// The report-wide coverage statement, preserved verbatim.
    pub report_coverage: DoctorReportCoverageV1,
    /// The canonical report's own coverage statement.
    pub note: String,
    pub presentation: DoctorReadPresentationV1,
}

/// Project an admitted canonical Doctor report for one closed finding family.
pub fn project_doctor_report(
    report: &DoctorReportV1,
    family_filter: Option<DoctorFindingFamilyV1>,
) -> Result<DoctorFindingsProjectionV1, DoctorProjectionErrorV1> {
    let entries = report
        .entries()
        .iter()
        .filter(|entry| family_filter.is_none_or(|family| entry.finding().family() == family))
        .cloned()
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Err(DoctorProjectionErrorV1::FamilyAbsent);
    }

    let coverage = family_coverage(report, family_filter, &entries);
    let domain_state = domain_state(&entries, &coverage);
    let freshness = freshness(&entries, domain_state);
    Ok(DoctorFindingsProjectionV1 {
        entries,
        report_coverage: report.coverage().clone(),
        note: report.coverage().statement().statement().to_owned(),
        presentation: DoctorReadPresentationV1 {
            domain_state,
            coverage,
            freshness,
            legal_actions: vec![doctor_findings_refresh_action()],
        },
    })
}

fn family_coverage(
    report: &DoctorReportV1,
    family_filter: Option<DoctorFindingFamilyV1>,
    entries: &[DoctorReportEntryV1],
) -> DashboardCoverageV1 {
    if let Some(family) = family_filter {
        let Some(family_coverage) = report
            .coverage()
            .families()
            .iter()
            .find(|coverage| coverage.family() == family)
        else {
            return DashboardCoverageV1::unknown();
        };
        return match family_coverage.consultation() {
            DoctorFamilyConsultationV1::Unavailable {
                reason:
                    DoctorFamilyUnavailableReasonV1::Unwired
                    | DoctorFamilyUnavailableReasonV1::Unsupported,
            } => DashboardCoverageV1::unsupported(),
            DoctorFamilyConsultationV1::Unavailable { .. } => DashboardCoverageV1::unknown(),
            DoctorFamilyConsultationV1::Consulted => {
                finding_coverage(entries, doctor_finding_family_label(family))
            }
        };
    }

    match report.coverage().completeness() {
        DoctorCoverageCompletenessV1::Complete => DashboardCoverageV1::complete(
            KNOWN_DOCTOR_FINDING_FAMILIES.len() as u64,
            "doctor_families",
        ),
        DoctorCoverageCompletenessV1::Partial => {
            let consulted = report
                .coverage()
                .families()
                .iter()
                .filter(|coverage| {
                    matches!(
                        coverage.consultation(),
                        DoctorFamilyConsultationV1::Consulted
                    )
                })
                .count() as u64;
            let omissions = report
                .coverage()
                .families()
                .iter()
                .filter_map(|coverage| match coverage.consultation() {
                    DoctorFamilyConsultationV1::Consulted => None,
                    DoctorFamilyConsultationV1::Unavailable { reason } => Some(format!(
                        "{}:{reason:?}",
                        doctor_finding_family_label(coverage.family())
                    )),
                })
                .collect();
            DashboardCoverageV1::partial(
                KNOWN_DOCTOR_FINDING_FAMILIES.len() as u64,
                consulted,
                "doctor_families",
                omissions,
            )
        }
        DoctorCoverageCompletenessV1::Unknown => {
            let unsupported = report.coverage().families().iter().all(|coverage| {
                matches!(
                    coverage.consultation(),
                    DoctorFamilyConsultationV1::Unavailable {
                        reason: DoctorFamilyUnavailableReasonV1::Unwired
                            | DoctorFamilyUnavailableReasonV1::Unsupported
                    }
                )
            });
            if unsupported {
                DashboardCoverageV1::unsupported()
            } else {
                DashboardCoverageV1::unknown()
            }
        }
    }
}

fn finding_coverage(entries: &[DoctorReportEntryV1], family: &'static str) -> DashboardCoverageV1 {
    if entries.iter().all(|entry| {
        entry.finding().coverage().completeness() == DoctorCoverageCompletenessV1::Complete
    }) {
        return DashboardCoverageV1::complete(entries.len() as u64, "doctor_findings");
    }
    if entries.iter().any(|entry| {
        entry.finding().coverage().completeness() == DoctorCoverageCompletenessV1::Partial
    }) {
        return DashboardCoverageV1::partial(
            entries.len() as u64,
            entries
                .iter()
                .filter(|entry| {
                    entry.finding().coverage().completeness()
                        == DoctorCoverageCompletenessV1::Complete
                })
                .count() as u64,
            "doctor_findings",
            vec![format!("{family}:partial")],
        );
    }
    DashboardCoverageV1::unknown()
}

fn domain_state(
    entries: &[DoctorReportEntryV1],
    coverage: &DashboardCoverageV1,
) -> DashboardDomainStateV1 {
    if coverage.is_complete()
        && entries
            .iter()
            .all(|entry| entry.finding().state().is_healthy_complete())
    {
        return DashboardDomainStateV1::Ready;
    }
    if entries
        .iter()
        .all(|entry| entry.finding().state() == DoctorEvidenceStateV1::Unsupported)
    {
        return DashboardDomainStateV1::Unsupported;
    }
    if entries
        .iter()
        .all(|entry| entry.finding().state() == DoctorEvidenceStateV1::Denied)
    {
        return DashboardDomainStateV1::Denied;
    }
    if entries
        .iter()
        .any(|entry| entry.finding().state() == DoctorEvidenceStateV1::Stale)
    {
        return DashboardDomainStateV1::Stale;
    }
    DashboardDomainStateV1::Partial
}

fn freshness(
    entries: &[DoctorReportEntryV1],
    domain_state: DashboardDomainStateV1,
) -> DashboardFreshnessV1 {
    unwatermarked_freshness(
        entries.iter().map(|entry| entry.finding().state()),
        domain_state,
    )
}

/// Project freshness for evidence that carries no observation timestamp or
/// source watermark. A report's receipt time is not evidence of source
/// freshness, so it must not be fabricated as a fresh observation.
fn unwatermarked_freshness(
    evidence_states: impl Iterator<Item = DoctorEvidenceStateV1>,
    domain_state: DashboardDomainStateV1,
) -> DashboardFreshnessV1 {
    if domain_state == DashboardDomainStateV1::Unsupported {
        return DashboardFreshnessV1::unsupported();
    }

    let mut has_stale_evidence = false;
    let mut all_evidence_absent = true;
    for state in evidence_states {
        has_stale_evidence |= state == DoctorEvidenceStateV1::Stale;
        all_evidence_absent &= state == DoctorEvidenceStateV1::Absent;
    }
    if has_stale_evidence {
        return DashboardFreshnessV1 {
            state: DashboardFreshnessStateV1::Stale,
            observed_at_micros: None,
            watermark: None,
        };
    }
    if all_evidence_absent {
        return DashboardFreshnessV1 {
            state: DashboardFreshnessStateV1::Absent,
            observed_at_micros: None,
            watermark: None,
        };
    }

    DashboardFreshnessV1::unknown()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_model::DashboardCoverageCompletenessV1;

    #[test]
    fn family_filter_parses_closed_vocabulary_and_rejects_unknown() {
        assert_eq!(parse_doctor_finding_family(None).unwrap(), None);
        assert_eq!(parse_doctor_finding_family(Some("")).unwrap(), None);
        assert_eq!(
            parse_doctor_finding_family(Some("storage")).unwrap(),
            Some(DoctorFindingFamilyV1::Storage)
        );
        assert_eq!(
            parse_doctor_finding_family(Some("storage_runtime")).unwrap(),
            Some(DoctorFindingFamilyV1::StorageRuntime)
        );
        assert_eq!(
            parse_doctor_finding_family(Some("nonsense")).unwrap_err(),
            "nonsense"
        );
    }

    #[test]
    fn every_known_family_has_a_label_that_round_trips() {
        for family in KNOWN_DOCTOR_FINDING_FAMILIES {
            let label = doctor_finding_family_label(family);
            assert_eq!(
                parse_doctor_finding_family(Some(label)).unwrap(),
                Some(family),
                "family label {label} must parse back to its own family"
            );
        }
    }

    #[test]
    fn absent_and_failed_sources_never_present_as_healthy_or_empty() {
        let unsupported = DoctorReadPresentationV1::source_unsupported();
        assert_eq!(
            unsupported.domain_state,
            DashboardDomainStateV1::Unsupported
        );
        assert_eq!(
            unsupported.coverage.completeness,
            DashboardCoverageCompletenessV1::Unsupported
        );
        assert!(!unsupported.coverage.is_complete());
        assert_eq!(
            unsupported.freshness.state,
            DashboardFreshnessStateV1::Unsupported
        );
        assert_eq!(
            unsupported.legal_actions,
            vec![doctor_findings_refresh_action()]
        );

        let failed = DoctorReadPresentationV1::source_failed();
        assert_eq!(failed.domain_state, DashboardDomainStateV1::Error);
        assert!(!failed.coverage.is_complete());
        assert_eq!(failed.freshness.state, DashboardFreshnessStateV1::Unknown);
        assert_eq!(failed.legal_actions, vec![doctor_findings_refresh_action()]);
    }

    #[test]
    fn projection_notes_name_the_exact_absence() {
        assert_eq!(
            DoctorProjectionErrorV1::FamilyAbsent.note(),
            "canonical Doctor report omitted the requested finding family"
        );
        assert_eq!(
            DoctorProjectionErrorV1::FamilyAbsent.to_string(),
            DoctorProjectionErrorV1::FamilyAbsent.note()
        );
    }

    #[test]
    fn source_failure_note_preserves_the_owner_error() {
        assert_eq!(
            doctor_report_failure_note(&"scope unavailable"),
            "Doctor report composition failed: scope unavailable"
        );
    }

    #[test]
    fn unwatermarked_evidence_never_fabricates_freshness() {
        for (evidence_state, domain_state) in [
            (
                DoctorEvidenceStateV1::HealthyCompleteCoverage,
                DashboardDomainStateV1::Ready,
            ),
            (
                DoctorEvidenceStateV1::Unknown,
                DashboardDomainStateV1::Partial,
            ),
            (
                DoctorEvidenceStateV1::Partial,
                DashboardDomainStateV1::Partial,
            ),
            (
                DoctorEvidenceStateV1::Denied,
                DashboardDomainStateV1::Denied,
            ),
            (
                DoctorEvidenceStateV1::Degraded,
                DashboardDomainStateV1::Partial,
            ),
        ] {
            let freshness = unwatermarked_freshness([evidence_state].into_iter(), domain_state);

            assert_eq!(freshness.state, DashboardFreshnessStateV1::Unknown);
            assert_eq!(freshness.observed_at_micros, None);
            assert_eq!(freshness.watermark, None);
        }
    }

    #[test]
    fn unwatermarked_evidence_preserves_absent_unsupported_and_stale_truth() {
        let absent = unwatermarked_freshness(
            [DoctorEvidenceStateV1::Absent].into_iter(),
            DashboardDomainStateV1::Partial,
        );
        assert_eq!(absent.state, DashboardFreshnessStateV1::Absent);
        assert_eq!(absent.observed_at_micros, None);
        assert_eq!(absent.watermark, None);

        let unsupported = unwatermarked_freshness(
            [DoctorEvidenceStateV1::Unsupported].into_iter(),
            DashboardDomainStateV1::Unsupported,
        );
        assert_eq!(unsupported.state, DashboardFreshnessStateV1::Unsupported);
        assert_eq!(unsupported.observed_at_micros, None);
        assert_eq!(unsupported.watermark, None);

        let stale = unwatermarked_freshness(
            [DoctorEvidenceStateV1::Stale].into_iter(),
            DashboardDomainStateV1::Stale,
        );
        assert_eq!(stale.state, DashboardFreshnessStateV1::Stale);
        assert_eq!(stale.observed_at_micros, None);
        assert_eq!(stale.watermark, None);
    }
}
