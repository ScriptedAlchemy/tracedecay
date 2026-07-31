//! Doctor report composition (Plan 09 §PR14, Plan 11 finding journey).
//!
//! [`DoctorReportComposerV1`] is the one entry point that gathers findings across
//! every [`DoctorFindingFamilyV1`] from the narrow source ports in
//! [`super::sources`], plus the landed storage producers, into a single
//! [`DoctorReportV1`]. It never evaluates a generic health score, never merges
//! findings by label, and never lets an unwired or unavailable family vanish: a
//! family with no wired source is carried with a truthful evidence state and an
//! explicit coverage entry, so the report's coverage statement always enumerates
//! which families were consulted versus unavailable.

use schemars::JsonSchema;
use serde::Serialize;

use crate::RequestContext;
use crate::error::ApplicationContractError;
use crate::storage::findings::truncate_at_char_boundary;

use super::sources::{
    AdvisoryFeedbackDoctorPort, CodeIndexMountDoctorPort, ConfigurationAuthorityDoctorPort,
    DoctorStorageFamilyReadV1, HostIntegrationDoctorPort, LanguageServerDoctorPort,
    ObservabilityDoctorPort, OperationalAuditDoctorPort, RuntimeHealthDoctorPort,
    StorageDoctorPort, advisory_feedback_findings, code_index_finding, configuration_finding,
    host_integration_finding, language_server_finding, observability_finding,
    operational_audit_findings, runtime_health_finding,
};
use super::types::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorStorageFindingKindV1,
};

/// Every finding family the Doctor report is contracted to consult, in a stable
/// order. A family absent from a composed report would be a silent omission; the
/// composer always emits an entry and a coverage record for each of these.
const REPORT_FAMILIES: [DoctorFindingFamilyV1; 7] = [
    DoctorFindingFamilyV1::Advisory,
    DoctorFindingFamilyV1::Configuration,
    DoctorFindingFamilyV1::StorageRuntime,
    DoctorFindingFamilyV1::Storage,
    DoctorFindingFamilyV1::LanguageServer,
    DoctorFindingFamilyV1::SemanticIndex,
    DoctorFindingFamilyV1::Observability,
];

/// The stable snake_case slug for a finding family, matching its serde encoding.
const fn family_slug(family: DoctorFindingFamilyV1) -> &'static str {
    match family {
        DoctorFindingFamilyV1::Advisory => "advisory",
        DoctorFindingFamilyV1::Configuration => "configuration",
        DoctorFindingFamilyV1::StorageRuntime => "storage_runtime",
        DoctorFindingFamilyV1::Storage => "storage",
        DoctorFindingFamilyV1::LanguageServer => "language_server",
        DoctorFindingFamilyV1::SemanticIndex => "semantic_index",
        DoctorFindingFamilyV1::Observability => "observability",
    }
}

/// Why a finding family could not be consulted from an observed source.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DoctorFamilyUnavailableReasonV1 {
    /// No source port is wired for this family in this composition.
    Unwired,
    /// The source is unsupported on this build/platform.
    Unsupported,
    /// The source is supported but produced nothing.
    Absent,
    /// The source read was denied.
    Denied,
    /// The source state could not be determined.
    Unknown,
}

impl DoctorFamilyUnavailableReasonV1 {
    /// The honest evidence state a synthesized placeholder finding carries for
    /// this unavailability reason.
    const fn evidence_state(self) -> DoctorEvidenceStateV1 {
        match self {
            // An unwired family is not supported by this composition build.
            Self::Unwired | Self::Unsupported => DoctorEvidenceStateV1::Unsupported,
            Self::Absent => DoctorEvidenceStateV1::Absent,
            Self::Denied => DoctorEvidenceStateV1::Denied,
            Self::Unknown => DoctorEvidenceStateV1::Unknown,
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Unwired => "unwired",
            Self::Unsupported => "unsupported",
            Self::Absent => "absent",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether a family was consulted from an observed source or is unavailable.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum DoctorFamilyConsultationV1 {
    /// A source produced observed evidence for this family.
    Consulted,
    /// No observed evidence; the family is carried as unavailable.
    Unavailable {
        reason: DoctorFamilyUnavailableReasonV1,
    },
}

impl DoctorFamilyConsultationV1 {
    #[must_use]
    const fn is_consulted(self) -> bool {
        matches!(self, Self::Consulted)
    }
}

/// The consultation status of one finding family within a report.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DoctorFamilyCoverageV1 {
    family: DoctorFindingFamilyV1,
    consultation: DoctorFamilyConsultationV1,
}

impl DoctorFamilyCoverageV1 {
    /// The finding family this record describes.
    #[must_use]
    pub fn family(&self) -> DoctorFindingFamilyV1 {
        self.family
    }

    /// Whether the family was consulted or is carried as unavailable.
    #[must_use]
    pub fn consultation(&self) -> DoctorFamilyConsultationV1 {
        self.consultation
    }
}

/// The report-wide coverage statement: which families were consulted versus
/// unavailable, plus an overall completeness and a bounded human statement.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DoctorReportCoverageV1 {
    families: Vec<DoctorFamilyCoverageV1>,
    completeness: DoctorCoverageCompletenessV1,
    statement: DoctorCoverageStatementV1,
}

impl DoctorReportCoverageV1 {
    /// Per-family consultation records, in stable family order.
    #[must_use]
    pub fn families(&self) -> &[DoctorFamilyCoverageV1] {
        &self.families
    }

    /// Overall coverage completeness across all families. `Complete` only when
    /// every family was consulted and every finding carries complete coverage.
    #[must_use]
    pub fn completeness(&self) -> DoctorCoverageCompletenessV1 {
        self.completeness
    }

    /// The bounded human-readable coverage statement.
    #[must_use]
    pub fn statement(&self) -> &DoctorCoverageStatementV1 {
        &self.statement
    }
}

/// One entry in a Doctor report: a canonical finding plus, for the storage
/// family, its typed subclass. The subclass is present only for storage findings
/// (a non-storage entry never carries one).
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DoctorReportEntryV1 {
    finding: DoctorFindingV1,
    storage_kind: Option<DoctorStorageFindingKindV1>,
}

impl DoctorReportEntryV1 {
    /// Construct a report entry. A storage subclass may be attached only to a
    /// `Storage`-family finding; attaching one to any other family is a contract
    /// error rather than a silent mislabel.
    fn new(
        finding: DoctorFindingV1,
        storage_kind: Option<DoctorStorageFindingKindV1>,
    ) -> Result<Self, ApplicationContractError> {
        if storage_kind.is_some() && finding.family() != DoctorFindingFamilyV1::Storage {
            return Err(ApplicationContractError::Inconsistent {
                field: "doctor report entry storage kind",
            });
        }
        Ok(Self {
            finding,
            storage_kind,
        })
    }

    /// The canonical finding.
    #[must_use]
    pub fn finding(&self) -> &DoctorFindingV1 {
        &self.finding
    }

    /// The typed storage subclass, present only for a storage finding.
    #[must_use]
    pub fn storage_kind(&self) -> Option<DoctorStorageFindingKindV1> {
        self.storage_kind
    }
}

/// One composed Doctor report.
///
/// The report carries one or more entries per family (findings are never merged
/// by label) plus a coverage statement that enumerates every family consulted
/// versus unavailable. Severity (the finding's evidence state) and evidence
/// quality (coverage completeness) are kept as separate dimensions: a degraded
/// finding with complete coverage does not weaken report completeness, and only
/// genuinely complete coverage with every finding healthy makes the whole report
/// assert health.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DoctorReportV1 {
    entries: Vec<DoctorReportEntryV1>,
    coverage: DoctorReportCoverageV1,
}

impl DoctorReportV1 {
    /// All report entries, in stable family order (never merged or deduplicated).
    #[must_use]
    pub fn entries(&self) -> &[DoctorReportEntryV1] {
        &self.entries
    }

    /// The report-wide coverage statement.
    #[must_use]
    pub fn coverage(&self) -> &DoctorReportCoverageV1 {
        &self.coverage
    }

    /// All findings the report carries, in stable order.
    pub fn findings(&self) -> impl Iterator<Item = &DoctorFindingV1> {
        self.entries.iter().map(DoctorReportEntryV1::finding)
    }

    /// Whether the whole report asserts a healthy, completely covered result.
    ///
    /// True only when every family was consulted with complete coverage and
    /// every finding is [`DoctorEvidenceStateV1::HealthyCompleteCoverage`]. Any
    /// unavailable family, partial coverage, or non-healthy finding makes this
    /// false — unknown or partial truth never collapses into a healthy report.
    #[must_use]
    pub fn is_healthy_complete(&self) -> bool {
        matches!(
            self.coverage.completeness,
            DoctorCoverageCompletenessV1::Complete
        ) && self
            .entries
            .iter()
            .all(|entry| entry.finding.state().is_healthy_complete())
    }
}

/// Compose one [`DoctorReportV1`] from the wired source ports.
///
/// Each source port is optional. A family whose port is absent (or whose source
/// reports an unavailable read) is carried with a truthful evidence state and an
/// explicit coverage entry — never silently omitted. Build the composer with the
/// `with_*` methods, then call [`Self::compose`].
#[derive(Default)]
pub struct DoctorReportComposerV1<'a> {
    configuration: Option<&'a dyn ConfigurationAuthorityDoctorPort>,
    runtime: Option<&'a dyn RuntimeHealthDoctorPort>,
    operational_audit: Option<&'a dyn OperationalAuditDoctorPort>,
    host: Option<&'a dyn HostIntegrationDoctorPort>,
    advisory_feedback: Option<&'a dyn AdvisoryFeedbackDoctorPort>,
    language_server: Option<&'a dyn LanguageServerDoctorPort>,
    code_index: Option<&'a dyn CodeIndexMountDoctorPort>,
    observability: Option<&'a dyn ObservabilityDoctorPort>,
    storage: Option<&'a dyn StorageDoctorPort>,
}

impl<'a> DoctorReportComposerV1<'a> {
    /// A composer with no wired sources. Composing it yields a truthful report in
    /// which every family is unavailable (unwired).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire the configuration authority source (Configuration family).
    #[must_use]
    pub fn with_configuration(mut self, port: &'a dyn ConfigurationAuthorityDoctorPort) -> Self {
        self.configuration = Some(port);
        self
    }

    /// Wire the daemon/runtime health source (StorageRuntime family).
    #[must_use]
    pub fn with_runtime(mut self, port: &'a dyn RuntimeHealthDoctorPort) -> Self {
        self.runtime = Some(port);
        self
    }

    /// Wire Remote HTTPS and exact registered-profile operational authority.
    #[must_use]
    pub fn with_operational_audit(mut self, port: &'a dyn OperationalAuditDoctorPort) -> Self {
        self.operational_audit = Some(port);
        self
    }

    /// Wire the host/agent integration conformance source (Advisory family).
    #[must_use]
    pub fn with_host(mut self, port: &'a dyn HostIntegrationDoctorPort) -> Self {
        self.host = Some(port);
        self
    }

    /// Wire the mounted canonical feedback read model (Advisory family).
    #[must_use]
    pub fn with_advisory_feedback(mut self, port: &'a dyn AdvisoryFeedbackDoctorPort) -> Self {
        self.advisory_feedback = Some(port);
        self
    }

    /// Wire live language-server/analyzer state (LanguageServer family).
    #[must_use]
    pub fn with_language_server(mut self, port: &'a dyn LanguageServerDoctorPort) -> Self {
        self.language_server = Some(port);
        self
    }

    /// Wire the code/semantic index mount source (SemanticIndex family).
    #[must_use]
    pub fn with_code_index(mut self, port: &'a dyn CodeIndexMountDoctorPort) -> Self {
        self.code_index = Some(port);
        self
    }

    /// Wire the canonical durable Plan-26 read model (Observability family).
    #[must_use]
    pub fn with_observability(mut self, port: &'a dyn ObservabilityDoctorPort) -> Self {
        self.observability = Some(port);
        self
    }

    /// Wire the storage retention/size source (Storage family).
    #[must_use]
    pub fn with_storage(mut self, port: &'a dyn StorageDoctorPort) -> Self {
        self.storage = Some(port);
        self
    }

    /// Gather findings across every family and assemble the report.
    pub async fn compose(
        &self,
        context: &RequestContext,
    ) -> Result<DoctorReportV1, ApplicationContractError> {
        let mut entries: Vec<DoctorReportEntryV1> = Vec::new();
        let mut coverage: Vec<DoctorFamilyCoverageV1> = Vec::new();

        for family in REPORT_FAMILIES {
            let (family_entries, consultation) = match family {
                DoctorFindingFamilyV1::Advisory => self.compose_advisory(context).await?,
                DoctorFindingFamilyV1::Configuration => self.compose_configuration(context).await?,
                DoctorFindingFamilyV1::StorageRuntime => self.compose_runtime(context).await?,
                DoctorFindingFamilyV1::Storage => self.compose_storage(context).await?,
                DoctorFindingFamilyV1::LanguageServer => {
                    self.compose_language_server(context).await?
                }
                DoctorFindingFamilyV1::SemanticIndex => self.compose_code_index(context).await?,
                DoctorFindingFamilyV1::Observability => self.compose_observability(context).await?,
            };
            entries.extend(family_entries);
            coverage.push(DoctorFamilyCoverageV1 {
                family,
                consultation,
            });
        }

        let report_coverage = build_coverage(coverage, &entries)?;
        Ok(DoctorReportV1 {
            entries,
            coverage: report_coverage,
        })
    }

    async fn compose_configuration(
        &self,
        context: &RequestContext,
    ) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError>
    {
        let Some(port) = self.configuration else {
            return unwired_family(DoctorFindingFamilyV1::Configuration);
        };
        let read = port.configuration_health(context).await;
        use super::sources::ConfigurationAuthorityReadV1 as Read;
        let consultation = match read {
            Read::Resolved { .. } => DoctorFamilyConsultationV1::Consulted,
            Read::Unsupported => unavailable(DoctorFamilyUnavailableReasonV1::Unsupported),
            Read::Absent => unavailable(DoctorFamilyUnavailableReasonV1::Absent),
            Read::Denied => unavailable(DoctorFamilyUnavailableReasonV1::Denied),
            Read::Unknown => unavailable(DoctorFamilyUnavailableReasonV1::Unknown),
        };
        let finding = configuration_finding(&read)?;
        Ok((vec![DoctorReportEntryV1::new(finding, None)?], consultation))
    }

    async fn compose_runtime(
        &self,
        context: &RequestContext,
    ) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError>
    {
        if self.runtime.is_none() && self.operational_audit.is_none() {
            return unwired_family(DoctorFindingFamilyV1::StorageRuntime);
        }
        let mut entries = Vec::new();
        let mut consultations = Vec::new();
        if let Some(port) = self.runtime {
            let read = port.runtime_health(context).await;
            use super::sources::RuntimeHealthReadV1 as Read;
            consultations.push(match read {
                Read::Observed { .. } => DoctorFamilyConsultationV1::Consulted,
                Read::Unsupported => unavailable(DoctorFamilyUnavailableReasonV1::Unsupported),
                Read::Absent => unavailable(DoctorFamilyUnavailableReasonV1::Absent),
                Read::Denied => unavailable(DoctorFamilyUnavailableReasonV1::Denied),
                Read::Unknown => unavailable(DoctorFamilyUnavailableReasonV1::Unknown),
            });
            entries.push(DoctorReportEntryV1::new(
                runtime_health_finding(&read)?,
                None,
            )?);
        }
        if let Some(port) = self.operational_audit {
            let read = port.operational_audit(context).await;
            use super::sources::{
                ProfileAuthorityReadV1 as Profile, RemoteOperationalReadV1 as Remote,
            };
            consultations.push(match (&read.remote, &read.profile_authority) {
                (Remote::Observed { .. }, _) | (_, Profile::Observed { .. }) => {
                    DoctorFamilyConsultationV1::Consulted
                }
                (Remote::Denied, _) | (_, Profile::Denied) => {
                    unavailable(DoctorFamilyUnavailableReasonV1::Denied)
                }
                (Remote::Unsupported, _) => {
                    unavailable(DoctorFamilyUnavailableReasonV1::Unsupported)
                }
                (Remote::Unconfigured, _) => unavailable(DoctorFamilyUnavailableReasonV1::Absent),
                (Remote::Unavailable, Profile::Unavailable) => {
                    unavailable(DoctorFamilyUnavailableReasonV1::Unknown)
                }
            });
            for finding in operational_audit_findings(&read)? {
                entries.push(DoctorReportEntryV1::new(finding, None)?);
            }
        }
        Ok((entries, strongest_consultation(consultations)))
    }

    async fn compose_host(
        &self,
        context: &RequestContext,
    ) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError>
    {
        let Some(port) = self.host else {
            return unwired_family(DoctorFindingFamilyV1::Advisory);
        };
        let read = port.host_conformance(context).await;
        use super::sources::HostIntegrationReadV1 as Read;
        let consultation = match read {
            Read::Observed { .. } => DoctorFamilyConsultationV1::Consulted,
            Read::Unsupported => unavailable(DoctorFamilyUnavailableReasonV1::Unsupported),
            Read::Absent => unavailable(DoctorFamilyUnavailableReasonV1::Absent),
            Read::Denied => unavailable(DoctorFamilyUnavailableReasonV1::Denied),
            Read::Unknown => unavailable(DoctorFamilyUnavailableReasonV1::Unknown),
        };
        let finding = host_integration_finding(&read)?;
        Ok((vec![DoctorReportEntryV1::new(finding, None)?], consultation))
    }

    async fn compose_advisory(
        &self,
        context: &RequestContext,
    ) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError>
    {
        if self.host.is_none() && self.advisory_feedback.is_none() {
            return unwired_family(DoctorFindingFamilyV1::Advisory);
        }
        let mut entries = Vec::new();
        let mut consultations = Vec::new();
        if self.host.is_some() {
            let (host_entries, host_consultation) = self.compose_host(context).await?;
            entries.extend(host_entries);
            consultations.push(host_consultation);
        }
        if let Some(port) = self.advisory_feedback {
            let read = port.advisory_feedback(context).await;
            use super::sources::AdvisoryFeedbackReadV1 as Read;
            let consultation = match &read {
                Read::Observed { .. } => DoctorFamilyConsultationV1::Consulted,
                Read::Absent => unavailable(DoctorFamilyUnavailableReasonV1::Absent),
                Read::Unsupported => unavailable(DoctorFamilyUnavailableReasonV1::Unsupported),
                Read::Denied => unavailable(DoctorFamilyUnavailableReasonV1::Denied),
                Read::Unknown => unavailable(DoctorFamilyUnavailableReasonV1::Unknown),
            };
            for finding in advisory_feedback_findings(&read)? {
                entries.push(DoctorReportEntryV1::new(finding, None)?);
            }
            consultations.push(consultation);
        }
        let consultation = consultations
            .iter()
            .copied()
            .find(|consultation| consultation.is_consulted())
            .unwrap_or_else(|| {
                consultations
                    .into_iter()
                    .max_by_key(|consultation| match consultation {
                        DoctorFamilyConsultationV1::Consulted => 5,
                        DoctorFamilyConsultationV1::Unavailable {
                            reason: DoctorFamilyUnavailableReasonV1::Unknown,
                        } => 4,
                        DoctorFamilyConsultationV1::Unavailable {
                            reason: DoctorFamilyUnavailableReasonV1::Denied,
                        } => 3,
                        DoctorFamilyConsultationV1::Unavailable {
                            reason: DoctorFamilyUnavailableReasonV1::Absent,
                        } => 2,
                        DoctorFamilyConsultationV1::Unavailable {
                            reason: DoctorFamilyUnavailableReasonV1::Unsupported,
                        } => 1,
                        DoctorFamilyConsultationV1::Unavailable {
                            reason: DoctorFamilyUnavailableReasonV1::Unwired,
                        } => 0,
                    })
                    .expect("at least one advisory port is wired")
            });
        Ok((entries, consultation))
    }

    async fn compose_code_index(
        &self,
        context: &RequestContext,
    ) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError>
    {
        let Some(port) = self.code_index else {
            return unwired_family(DoctorFindingFamilyV1::SemanticIndex);
        };
        let read = port.code_index_mount(context).await;
        use super::sources::CodeIndexMountReadV1 as Read;
        let consultation = match read {
            Read::Observed { .. } => DoctorFamilyConsultationV1::Consulted,
            Read::Unsupported => unavailable(DoctorFamilyUnavailableReasonV1::Unsupported),
            Read::Absent => unavailable(DoctorFamilyUnavailableReasonV1::Absent),
            Read::Denied => unavailable(DoctorFamilyUnavailableReasonV1::Denied),
            Read::Unknown => unavailable(DoctorFamilyUnavailableReasonV1::Unknown),
        };
        let finding = code_index_finding(&read)?;
        Ok((vec![DoctorReportEntryV1::new(finding, None)?], consultation))
    }

    async fn compose_language_server(
        &self,
        context: &RequestContext,
    ) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError>
    {
        let Some(port) = self.language_server else {
            return unwired_family(DoctorFindingFamilyV1::LanguageServer);
        };
        let read = port.language_server_health(context).await;
        use super::sources::LanguageServerReadV1 as Read;
        let consultation = match read {
            Read::Observed { .. } => DoctorFamilyConsultationV1::Consulted,
            Read::Unsupported => unavailable(DoctorFamilyUnavailableReasonV1::Unsupported),
            Read::Absent => unavailable(DoctorFamilyUnavailableReasonV1::Absent),
            Read::Denied => unavailable(DoctorFamilyUnavailableReasonV1::Denied),
            Read::Unknown => unavailable(DoctorFamilyUnavailableReasonV1::Unknown),
        };
        let finding = language_server_finding(&read)?;
        Ok((vec![DoctorReportEntryV1::new(finding, None)?], consultation))
    }

    async fn compose_observability(
        &self,
        context: &RequestContext,
    ) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError>
    {
        let Some(port) = self.observability else {
            return unwired_family(DoctorFindingFamilyV1::Observability);
        };
        let read = port.observability_health(context).await;
        use super::sources::ObservabilityReadV1 as Read;
        let consultation = match read {
            Read::Observed { .. } => DoctorFamilyConsultationV1::Consulted,
            Read::Unsupported => unavailable(DoctorFamilyUnavailableReasonV1::Unsupported),
            Read::Absent => unavailable(DoctorFamilyUnavailableReasonV1::Absent),
            Read::Denied => unavailable(DoctorFamilyUnavailableReasonV1::Denied),
            Read::Unknown => unavailable(DoctorFamilyUnavailableReasonV1::Unknown),
        };
        let finding = observability_finding(&read)?;
        Ok((vec![DoctorReportEntryV1::new(finding, None)?], consultation))
    }

    async fn compose_storage(
        &self,
        context: &RequestContext,
    ) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError>
    {
        let Some(port) = self.storage else {
            return unwired_family(DoctorFindingFamilyV1::Storage);
        };
        let read = port.storage_findings(context).await;
        match read {
            DoctorStorageFamilyReadV1::Observed { findings } if !findings.is_empty() => {
                let mut family_entries = Vec::with_capacity(findings.len());
                for typed in findings {
                    let kind = typed.kind();
                    family_entries
                        .push(DoctorReportEntryV1::new(typed.into_finding(), Some(kind))?);
                }
                Ok((family_entries, DoctorFamilyConsultationV1::Consulted))
            }
            // An empty observed read means the runtime produced no storage
            // findings; that is an absent family, not a healthy claim.
            DoctorStorageFamilyReadV1::Observed { .. } | DoctorStorageFamilyReadV1::Absent => {
                storage_unavailable(DoctorFamilyUnavailableReasonV1::Absent)
            }
            DoctorStorageFamilyReadV1::Unsupported => {
                storage_unavailable(DoctorFamilyUnavailableReasonV1::Unsupported)
            }
            DoctorStorageFamilyReadV1::Denied => {
                storage_unavailable(DoctorFamilyUnavailableReasonV1::Denied)
            }
            DoctorStorageFamilyReadV1::Unknown => {
                storage_unavailable(DoctorFamilyUnavailableReasonV1::Unknown)
            }
        }
    }
}

/// A consultation record for an unavailable family.
const fn unavailable(reason: DoctorFamilyUnavailableReasonV1) -> DoctorFamilyConsultationV1 {
    DoctorFamilyConsultationV1::Unavailable { reason }
}

fn strongest_consultation(
    consultations: Vec<DoctorFamilyConsultationV1>,
) -> DoctorFamilyConsultationV1 {
    consultations
        .iter()
        .copied()
        .find(|consultation| consultation.is_consulted())
        .unwrap_or_else(|| {
            consultations
                .into_iter()
                .max_by_key(|consultation| match consultation {
                    DoctorFamilyConsultationV1::Consulted => 5,
                    DoctorFamilyConsultationV1::Unavailable {
                        reason: DoctorFamilyUnavailableReasonV1::Unknown,
                    } => 4,
                    DoctorFamilyConsultationV1::Unavailable {
                        reason: DoctorFamilyUnavailableReasonV1::Denied,
                    } => 3,
                    DoctorFamilyConsultationV1::Unavailable {
                        reason: DoctorFamilyUnavailableReasonV1::Absent,
                    } => 2,
                    DoctorFamilyConsultationV1::Unavailable {
                        reason: DoctorFamilyUnavailableReasonV1::Unsupported,
                    } => 1,
                    DoctorFamilyConsultationV1::Unavailable {
                        reason: DoctorFamilyUnavailableReasonV1::Unwired,
                    } => 0,
                })
                .expect("a composed family has at least one source")
        })
}

/// Synthesize the single placeholder entry for a family with no wired source.
fn unwired_family(
    family: DoctorFindingFamilyV1,
) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError> {
    let finding = placeholder_finding(family, DoctorFamilyUnavailableReasonV1::Unwired)?;
    Ok((
        vec![DoctorReportEntryV1::new(finding, None)?],
        unavailable(DoctorFamilyUnavailableReasonV1::Unwired),
    ))
}

/// Synthesize the single placeholder entry for an unavailable storage read.
fn storage_unavailable(
    reason: DoctorFamilyUnavailableReasonV1,
) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError> {
    let finding = placeholder_finding(DoctorFindingFamilyV1::Storage, reason)?;
    // The placeholder carries no storage subclass: no specific condition was
    // observed, so there is nothing to classify.
    Ok((
        vec![DoctorReportEntryV1::new(finding, None)?],
        unavailable(reason),
    ))
}

/// Build a truthful non-healthy placeholder finding for an unavailable family.
fn placeholder_finding(
    family: DoctorFindingFamilyV1,
    reason: DoctorFamilyUnavailableReasonV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let reference = format!("doctor.{}.{}", family_slug(family), reason.slug());
    let evidence = DoctorEvidenceRefV1::new(family, DoctorEvidenceReferenceV1::new(reference)?);
    let statement = format!(
        "{} family source unavailable ({})",
        family_slug(family),
        reason.slug()
    );
    DoctorFindingV1::new(
        family,
        reason.evidence_state(),
        vec![evidence],
        DoctorCoverageStatementV1::new(DoctorCoverageCompletenessV1::Unknown, statement)?,
        None,
    )
}

/// Assemble the report-wide coverage statement from per-family records.
fn build_coverage(
    families: Vec<DoctorFamilyCoverageV1>,
    entries: &[DoctorReportEntryV1],
) -> Result<DoctorReportCoverageV1, ApplicationContractError> {
    let total = families.len();
    let consulted = families
        .iter()
        .filter(|record| record.consultation.is_consulted())
        .count();
    let all_findings_complete = entries
        .iter()
        .all(|entry| entry.finding.coverage().is_complete());
    // Coverage completeness is about *observation*, not health: complete only
    // when every family was consulted and every finding carries complete
    // coverage. Severity is carried independently on each finding.
    let completeness = if consulted == total && all_findings_complete {
        DoctorCoverageCompletenessV1::Complete
    } else {
        DoctorCoverageCompletenessV1::Partial
    };

    let statement = build_statement(&families, consulted, total);
    Ok(DoctorReportCoverageV1 {
        families,
        completeness,
        statement: DoctorCoverageStatementV1::new(completeness, statement)?,
    })
}

/// Build the bounded human-readable coverage statement enumerating unavailable
/// families. Kept within the 512-byte coverage-statement budget.
fn build_statement(families: &[DoctorFamilyCoverageV1], consulted: usize, total: usize) -> String {
    let mut unavailable_list = String::new();
    for record in families {
        if let DoctorFamilyConsultationV1::Unavailable { reason } = record.consultation {
            if !unavailable_list.is_empty() {
                unavailable_list.push_str(", ");
            }
            unavailable_list.push_str(family_slug(record.family));
            unavailable_list.push('(');
            unavailable_list.push_str(reason.slug());
            unavailable_list.push(')');
        }
    }
    let mut statement = format!("consulted {consulted}/{total} doctor finding families");
    if unavailable_list.is_empty() {
        statement.push_str("; all families consulted");
    } else {
        statement.push_str("; unavailable: ");
        statement.push_str(&unavailable_list);
    }
    // The family/reason vocabulary is closed and small, so the composed
    // statement is well within the 512-byte budget; guard defensively anyway.
    truncate_at_char_boundary(&statement, 512)
}
