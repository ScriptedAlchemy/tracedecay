//! Doctor kernel.
//!
//! The transport-neutral Doctor application kernel: typed finding families,
//! evidence states, coverage, the narrow source ports each authority is reached
//! through, and the one composition entry point ([`DoctorReportComposerV1`])
//! that gathers findings across every family into a [`DoctorReportV1`]. This
//! module owns no store, transport, provider runtime, or health formula;
//! source-port implementations and any surface binding are owned elsewhere.

mod report;
mod sources;
mod types;

pub use report::{
    DOCTOR_FINDING_FAMILIES, DoctorFamilyConsultationV1, DoctorFamilyCoverageV1,
    DoctorFamilyUnavailableReasonV1, DoctorReportComposerV1, DoctorReportCoverageV1,
    DoctorReportEntryV1, DoctorReportV1, doctor_finding_family_label,
};
pub use sources::{
    AdvisoryFeedbackDoctorPort, AdvisoryFeedbackFindingReadV1, AdvisoryFeedbackReadV1,
    AdvisoryFeedbackSummaryReadV1, CodeIndexMountDoctorPort, CodeIndexMountReadV1,
    CodeIndexMountStateV1, ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1,
    ConfigurationDriftV1, DoctorSourceFuture, DoctorStorageFamilyReadV1,
    DoctorStorageIncompleteReasonV1, HostConformanceV1, HostIntegrationDoctorPort,
    HostIntegrationReadV1, IngestRefusalCensusReadV1, IngestRefusalCountV1,
    LanguageServerDoctorPort, LanguageServerReadV1, LanguageServerStateV1, ObservabilityDoctorPort,
    ObservabilityReadV1, ObservabilityStateV1, OperationalAuditDoctorPort, OperationalAuditReadV1,
    ProfileAuthorityReadV1, RemoteAuthorityReadV1, RemoteListenerReadV1, RemoteOperationalReadV1,
    RuntimeHealthDoctorPort, RuntimeHealthReadV1, RuntimeLivenessV1, StorageDoctorPort,
    advisory_feedback_findings, code_index_finding, configuration_finding,
    host_integration_finding, ingest_refusal_finding, language_server_finding,
    observability_finding, operational_audit_findings, runtime_health_finding,
};
pub use types::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorStorageFindingKindV1, DoctorStorageFindingV1,
};
