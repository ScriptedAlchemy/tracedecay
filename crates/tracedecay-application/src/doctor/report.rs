//! Doctor report composition.
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
use serde::{Deserialize, Deserializer, Serialize};

use crate::RequestContext;
use crate::error::ApplicationContractError;
use crate::storage::findings::truncate_at_char_boundary;

use super::sources::{
    AdvisoryFeedbackDoctorPort, CodeIndexMountDoctorPort, ConfigurationAuthorityDoctorPort,
    DoctorStorageFamilyReadV1, DoctorStorageIncompleteReasonV1, HostIntegrationDoctorPort,
    LanguageServerDoctorPort, ObservabilityDoctorPort, OperationalAuditDoctorPort,
    RuntimeHealthDoctorPort, StorageDoctorPort, advisory_feedback_findings, code_index_finding,
    configuration_finding, host_integration_finding, language_server_finding,
    observability_finding, operational_audit_findings, runtime_health_finding,
};
use super::types::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorStorageFindingKindV1, DoctorStorageFindingV1,
};

/// Every finding family the Doctor report is contracted to consult, in a stable
/// order. A family absent from a composed report would be a silent omission; the
/// composer always emits an entry and a coverage record for each of these.
pub const DOCTOR_FINDING_FAMILIES: [DoctorFindingFamilyV1; 7] = [
    DoctorFindingFamilyV1::Advisory,
    DoctorFindingFamilyV1::Configuration,
    DoctorFindingFamilyV1::StorageRuntime,
    DoctorFindingFamilyV1::Storage,
    DoctorFindingFamilyV1::LanguageServer,
    DoctorFindingFamilyV1::SemanticIndex,
    DoctorFindingFamilyV1::Observability,
];

/// The stable snake_case slug for a finding family, matching its serde encoding.
pub const fn doctor_finding_family_label(family: DoctorFindingFamilyV1) -> &'static str {
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

/// Why complete coverage of a finding family was unavailable.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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
    /// The source could not be reached. Named separately from `Unknown`: the
    /// source is identified and its unreachability is an observation, not an
    /// undetermined state.
    Unavailable,
    /// The source must be rebuilt before it can be read again.
    ResetRequired,
    /// The source was read and found corrupt.
    Corrupt,
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
            // An unreachable source is genuinely undetermined; a corrupt or
            // reset-required source is an OBSERVED degraded condition, which is
            // a stronger claim than "could not be determined".
            Self::Unknown | Self::Unavailable => DoctorEvidenceStateV1::Unknown,
            Self::ResetRequired | Self::Corrupt => DoctorEvidenceStateV1::Degraded,
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Unwired => "unwired",
            Self::Unsupported => "unsupported",
            Self::Absent => "absent",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
            Self::Unavailable => "unavailable",
            Self::ResetRequired => "reset_required",
            Self::Corrupt => "corrupt",
        }
    }
}

/// Whether a family was consulted from an observed source or is unavailable.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum DoctorFamilyConsultationV1 {
    /// A source produced observed evidence for this family.
    Consulted,
    /// Complete family coverage was unavailable. Storage may still carry
    /// findings from resolved producers when an independent producer failed.
    Unavailable {
        reason: DoctorFamilyUnavailableReasonV1,
    },
}

impl DoctorFamilyConsultationV1 {
    #[must_use]
    const fn is_consulted(self) -> bool {
        matches!(self, Self::Consulted)
    }

    /// How strongly this consultation record speaks, used to pick the surviving
    /// record when several independent sources composed one family.
    ///
    /// A NAMED degradation outranks a bare `Unknown`: a source that explained
    /// why it is unavailable must not be masked by a peer that merely could not
    /// be determined. This is the single ranking the composer uses everywhere.
    const fn rank(self) -> u8 {
        match self {
            Self::Consulted => 8,
            Self::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Corrupt,
            } => 7,
            Self::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::ResetRequired,
            } => 6,
            Self::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Unavailable,
            } => 5,
            Self::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Unknown,
            } => 4,
            Self::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Denied,
            } => 3,
            Self::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Absent,
            } => 2,
            Self::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Unsupported,
            } => 1,
            Self::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Unwired,
            } => 0,
        }
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

impl<'de> Deserialize<'de> for DoctorReportV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct EntryWire {
            finding: DoctorFindingV1,
            storage_kind: Option<DoctorStorageFindingKindV1>,
        }

        #[derive(Deserialize)]
        struct FamilyCoverageWire {
            family: DoctorFindingFamilyV1,
            consultation: DoctorFamilyConsultationV1,
        }

        #[derive(Deserialize)]
        struct CoverageWire {
            families: Vec<FamilyCoverageWire>,
            completeness: DoctorCoverageCompletenessV1,
            statement: DoctorCoverageStatementV1,
        }

        #[derive(Deserialize)]
        struct ReportWire {
            entries: Vec<EntryWire>,
            coverage: CoverageWire,
        }

        let wire = ReportWire::deserialize(deserializer)?;
        let entries = wire
            .entries
            .into_iter()
            .map(|entry| DoctorReportEntryV1::new(entry.finding, entry.storage_kind))
            .collect::<Result<Vec<_>, _>>()
            .map_err(serde::de::Error::custom)?;
        let families = wire
            .coverage
            .families
            .into_iter()
            .map(|coverage| DoctorFamilyCoverageV1 {
                family: coverage.family,
                consultation: coverage.consultation,
            })
            .collect::<Vec<_>>();

        if families.len() != DOCTOR_FINDING_FAMILIES.len()
            || families
                .iter()
                .zip(DOCTOR_FINDING_FAMILIES)
                .any(|(coverage, expected)| coverage.family != expected)
            || entries.is_empty()
            || DOCTOR_FINDING_FAMILIES.iter().any(|expected| {
                !entries
                    .iter()
                    .any(|entry| entry.finding.family() == *expected)
            })
        {
            return Err(serde::de::Error::custom(
                "Doctor report omitted, duplicated, or reordered a required family",
            ));
        }

        let expected_coverage =
            build_coverage(families, &entries).map_err(serde::de::Error::custom)?;
        if expected_coverage.completeness != wire.coverage.completeness
            || expected_coverage.statement != wire.coverage.statement
        {
            return Err(serde::de::Error::custom(
                "Doctor report coverage contradicted its findings or consultations",
            ));
        }

        Ok(Self {
            entries,
            coverage: expected_coverage,
        })
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

    /// Wire the canonical durable feedback read model (Observability family).
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

        for family in DOCTOR_FINDING_FAMILIES {
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
                    .max_by_key(|consultation| consultation.rank())
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
            DoctorStorageFamilyReadV1::Observed { findings } if !findings.is_empty() => Ok((
                storage_entries(findings)?,
                DoctorFamilyConsultationV1::Consulted,
            )),
            DoctorStorageFamilyReadV1::ObservedIncomplete { findings, reason }
                if !findings.is_empty() =>
            {
                Ok((
                    storage_entries(findings)?,
                    unavailable(storage_incomplete_reason(&reason)),
                ))
            }
            // An empty observed read means the runtime produced no storage
            // findings; that is an absent family, not a healthy claim.
            DoctorStorageFamilyReadV1::Observed { .. } | DoctorStorageFamilyReadV1::Absent => {
                storage_unavailable(DoctorFamilyUnavailableReasonV1::Absent, None)
            }
            DoctorStorageFamilyReadV1::ObservedIncomplete { reason, .. } => storage_unavailable(
                storage_incomplete_reason(&reason),
                storage_incomplete_detail(&reason),
            ),
            DoctorStorageFamilyReadV1::Unsupported => {
                storage_unavailable(DoctorFamilyUnavailableReasonV1::Unsupported, None)
            }
            DoctorStorageFamilyReadV1::Denied => {
                storage_unavailable(DoctorFamilyUnavailableReasonV1::Denied, None)
            }
            DoctorStorageFamilyReadV1::Unknown => {
                storage_unavailable(DoctorFamilyUnavailableReasonV1::Unknown, None)
            }
            // The three named degradations carry the reason the storage source
            // reported. It is reproduced in the placeholder finding rather than
            // discarded, so the report says WHY the family is unavailable.
            DoctorStorageFamilyReadV1::Unavailable { detail } => storage_unavailable(
                DoctorFamilyUnavailableReasonV1::Unavailable,
                Some(detail.as_str()),
            ),
            DoctorStorageFamilyReadV1::ResetRequired { detail } => storage_unavailable(
                DoctorFamilyUnavailableReasonV1::ResetRequired,
                Some(detail.as_str()),
            ),
            DoctorStorageFamilyReadV1::Corrupt { detail } => storage_unavailable(
                DoctorFamilyUnavailableReasonV1::Corrupt,
                Some(detail.as_str()),
            ),
        }
    }
}

fn storage_entries(
    findings: Vec<DoctorStorageFindingV1>,
) -> Result<Vec<DoctorReportEntryV1>, ApplicationContractError> {
    findings
        .into_iter()
        .map(|typed| {
            let kind = typed.kind();
            DoctorReportEntryV1::new(typed.into_finding(), Some(kind))
        })
        .collect()
}

const fn storage_incomplete_reason(
    reason: &DoctorStorageIncompleteReasonV1,
) -> DoctorFamilyUnavailableReasonV1 {
    match reason {
        DoctorStorageIncompleteReasonV1::Unsupported => {
            DoctorFamilyUnavailableReasonV1::Unsupported
        }
        DoctorStorageIncompleteReasonV1::Denied => DoctorFamilyUnavailableReasonV1::Denied,
        DoctorStorageIncompleteReasonV1::Unknown => DoctorFamilyUnavailableReasonV1::Unknown,
        DoctorStorageIncompleteReasonV1::Unavailable { .. } => {
            DoctorFamilyUnavailableReasonV1::Unavailable
        }
        DoctorStorageIncompleteReasonV1::ResetRequired { .. } => {
            DoctorFamilyUnavailableReasonV1::ResetRequired
        }
        DoctorStorageIncompleteReasonV1::Corrupt { .. } => DoctorFamilyUnavailableReasonV1::Corrupt,
    }
}

/// The observed reason text a named storage degradation carries, if any.
const fn storage_incomplete_detail(reason: &DoctorStorageIncompleteReasonV1) -> Option<&str> {
    match reason {
        DoctorStorageIncompleteReasonV1::Unsupported
        | DoctorStorageIncompleteReasonV1::Denied
        | DoctorStorageIncompleteReasonV1::Unknown => None,
        DoctorStorageIncompleteReasonV1::Unavailable { detail }
        | DoctorStorageIncompleteReasonV1::ResetRequired { detail }
        | DoctorStorageIncompleteReasonV1::Corrupt { detail } => Some(detail.as_str()),
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
                .max_by_key(|consultation| consultation.rank())
                .expect("a composed family has at least one source")
        })
}

/// Synthesize the single placeholder entry for a family with no wired source.
fn unwired_family(
    family: DoctorFindingFamilyV1,
) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError> {
    let finding = placeholder_finding(family, DoctorFamilyUnavailableReasonV1::Unwired, None)?;
    Ok((
        vec![DoctorReportEntryV1::new(finding, None)?],
        unavailable(DoctorFamilyUnavailableReasonV1::Unwired),
    ))
}

/// Synthesize the single placeholder entry for an unavailable storage read.
fn storage_unavailable(
    reason: DoctorFamilyUnavailableReasonV1,
    detail: Option<&str>,
) -> Result<(Vec<DoctorReportEntryV1>, DoctorFamilyConsultationV1), ApplicationContractError> {
    let finding = placeholder_finding(DoctorFindingFamilyV1::Storage, reason, detail)?;
    // The placeholder carries no storage subclass: no specific condition was
    // observed, so there is nothing to classify.
    Ok((
        vec![DoctorReportEntryV1::new(finding, None)?],
        unavailable(reason),
    ))
}

/// The observed reason text a source reported, rendered safe for a coverage
/// statement: control characters folded to spaces, trimmed, and bounded so the
/// composed statement stays inside the statement contract.
fn sanitized_detail(detail: &str) -> Option<String> {
    let folded: String = detail
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let bounded = truncate_at_char_boundary(folded.trim(), PLACEHOLDER_DETAIL_MAX_BYTES);
    let bounded = bounded.trim();
    if bounded.is_empty() {
        None
    } else {
        Some(bounded.to_owned())
    }
}

/// Byte budget for the reason text inside a placeholder coverage statement.
/// The statement contract bounds the whole string at 512 bytes; the fixed
/// prefix is far shorter than the remaining headroom.
const PLACEHOLDER_DETAIL_MAX_BYTES: usize = 320;

/// Build a truthful non-healthy placeholder finding for an unavailable family.
///
/// When the source reported a reason, it is reproduced in the coverage
/// statement: a named degradation that discards its reason is only marginally
/// better than the bare `unknown` it replaced.
fn placeholder_finding(
    family: DoctorFindingFamilyV1,
    reason: DoctorFamilyUnavailableReasonV1,
    detail: Option<&str>,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let reference = format!(
        "doctor.{}.{}",
        doctor_finding_family_label(family),
        reason.slug()
    );
    let evidence = DoctorEvidenceRefV1::new(family, DoctorEvidenceReferenceV1::new(reference)?);
    let statement = match detail.and_then(sanitized_detail) {
        Some(detail) => format!(
            "{} family source unavailable ({}): {detail}",
            doctor_finding_family_label(family),
            reason.slug()
        ),
        None => format!(
            "{} family source unavailable ({})",
            doctor_finding_family_label(family),
            reason.slug()
        ),
    };
    DoctorFindingV1::new(
        family,
        reason.evidence_state(),
        vec![evidence],
        DoctorCoverageStatementV1::new(DoctorCoverageCompletenessV1::Unknown, statement)?,
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
            unavailable_list.push_str(doctor_finding_family_label(record.family));
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
