//! Semantic activation coordination port consumed by the configuration
//! control plane.
//!
//! This trait is only the methods the configuration runtime invokes on an
//! already-authorized semantic coordinator. It does not select profiles,
//! expose inventory stores, or mount a transport. Associated types keep
//! retrieval and store payloads in the crates that own them so this
//! ports-and-contracts crate does not take a `tracedecay-application` or
//! `tracedecay-search-eval` edge.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{ManifestDigest, UtcMicros};

#[derive(Clone, Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SemanticQualificationFailureV1 {
    #[error(
        "packaged PASS for profile {profile_id} is stale: packaged workload \
         {packaged_workload_digest}, current workload {current_workload_digest}, evidence \
         {evidence_digest}; {remedy}"
    )]
    StaleWorkload {
        profile_id: String,
        packaged_workload_digest: String,
        current_workload_digest: String,
        evidence_digest: String,
        remedy: String,
    },
    #[error(
        "native qualification evidence {evidence_digest} for profile {profile_id} and workload \
         {workload_digest} did not pass; {remedy}"
    )]
    FailedQualification {
        profile_id: String,
        workload_digest: String,
        evidence_digest: String,
        remedy: String,
    },
    /// The packaged bytes predate the current asset schema, so they cannot be
    /// read as evidence at all — distinct from evidence that was read and
    /// refused. The methodology version is unreadable here because it lives
    /// inside the shape that failed to decode.
    #[error(
        "qualification evidence for profile {profile_id} was written under packaged schema \
         {packaged_schema_version}, which this build superseded with schema \
         {current_schema_version}; {remedy}"
    )]
    SupersededSchema {
        profile_id: String,
        packaged_schema_version: u32,
        current_schema_version: u32,
        evidence_digest: Option<String>,
        remedy: String,
    },
    /// The packaged bytes decode, but they were scored under a decision rule
    /// this build no longer implements. Reinterpreting them under the current
    /// rule would assert a measurement nobody made.
    #[error(
        "qualification evidence for profile {profile_id} was scored under methodology \
         {packaged_methodology_version}, and this build decides under methodology \
         {current_methodology_version}; {remedy}"
    )]
    SupersededMethodology {
        profile_id: String,
        packaged_methodology_version: u32,
        current_methodology_version: u32,
        evidence_digest: Option<String>,
        remedy: String,
    },
    #[error(
        "no valid qualification evidence for profile {profile_id} and workload \
         {current_workload_digest}: {detail}; {remedy}"
    )]
    NoQualificationEvidence {
        profile_id: String,
        current_workload_digest: String,
        evidence_digest: Option<String>,
        detail: String,
        remedy: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SemanticQualificationStateV1 {
    Qualified {
        profile_id: String,
        workload_digest: String,
        evidence_digest: String,
    },
    Unqualified {
        failure: SemanticQualificationFailureV1,
    },
}

/// Typed failure for one configuration-linked semantic activation or rollback.
///
/// The `Runtime` payload is a display string so this crate does not name the
/// semantic-runtime control error. Implementors map that error at the
/// coordinator boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticActivationCoordinationErrorV1 {
    #[error("semantic activation configuration authority is unavailable")]
    Unavailable,
    #[error("semantic activation input was rejected")]
    Rejected,
    #[error("semantic activation input was rejected: {0}")]
    RejectedDetail(String),
    #[error("semantic activation qualification refused: {0}")]
    Qualification(Box<SemanticQualificationFailureV1>),
    #[error("semantic activation compare-and-swap conflicted")]
    Conflict,
    #[error("semantic runtime activation failed: {0}")]
    Runtime(String),
}

impl From<SemanticQualificationFailureV1> for SemanticActivationCoordinationErrorV1 {
    fn from(failure: SemanticQualificationFailureV1) -> Self {
        Self::Qualification(Box::new(failure))
    }
}

/// Coordination surface the configuration runtime actually calls.
///
/// Method list is the production call set from
/// `ProjectConfigurationRuntime` and the configuration operation that
/// reaches the installed coordinator through that runtime:
/// `bootstrap_query_profile`, `current_profile_state`,
/// `preview_central_mutation`, `stage_and_activate`, `stage_and_rollback`.
pub trait SemanticActivationCoordinationPort: Send + Sync {
    type ConfigurationState: Send + 'static;
    type AcceptedProfile: Send + 'static;
    type RuntimeCompatibility: Send + Sync + 'static;
    type ConfigurationPin: Send + 'static;
    type MutationCapability: Send + Sync + 'static;
    type ProfileCas: Send + 'static;
    type CentralMutation: Send + 'static;
    type ActivationReceipt: Send + 'static;
    type RollbackReceipt: Send + 'static;
    type ProfileState: Send + 'static;
    type MutationAuthority: Send + Sync + 'static;
    type PreviewOutcome: Send + 'static;

    fn bootstrap_query_profile<'a>(
        &'a self,
        configuration: Self::ConfigurationState,
        accepted_query: Self::AcceptedProfile,
        runtime: &'a Self::RuntimeCompatibility,
    ) -> Pin<Box<dyn Future<Output = Result<(), SemanticActivationCoordinationErrorV1>> + Send + 'a>>;

    fn current_profile_state<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Self::ProfileState, SemanticActivationCoordinationErrorV1>>
                + Send
                + 'a,
        >,
    >;

    fn preview_central_mutation<'a>(
        &'a self,
        authority: &'a Self::MutationAuthority,
        mutation: &'a Self::CentralMutation,
        expected_revision: &'a ConfigurationRevisionId,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Self::PreviewOutcome, SemanticActivationCoordinationErrorV1>>
                + Send
                + 'a,
        >,
    >;

    #[allow(clippy::too_many_arguments)]
    fn stage_and_activate<'a>(
        &'a self,
        base_configuration: Self::ConfigurationPin,
        result_configuration: Self::ConfigurationState,
        capability: &'a Self::MutationCapability,
        expected: Self::ProfileCas,
        candidate: Self::AcceptedProfile,
        current_runtime: &'a Self::RuntimeCompatibility,
        candidate_runtime: &'a Self::RuntimeCompatibility,
        central_mutation: Self::CentralMutation,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Self::ActivationReceipt, SemanticActivationCoordinationErrorV1>,
                > + Send
                + 'a,
        >,
    >;

    #[allow(clippy::too_many_arguments)]
    fn stage_and_rollback<'a>(
        &'a self,
        base_configuration: Self::ConfigurationPin,
        result_configuration: Self::ConfigurationState,
        capability: &'a Self::MutationCapability,
        expected: Self::ProfileCas,
        restored_runtime: &'a Self::RuntimeCompatibility,
        central_mutation: Self::CentralMutation,
        trigger: String,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Self::RollbackReceipt, SemanticActivationCoordinationErrorV1>,
                > + Send
                + 'a,
        >,
    >;
}
