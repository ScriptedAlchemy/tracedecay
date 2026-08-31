//! Semantic activation coordination port consumed by the configuration
//! control plane.
//!
//! This trait is only the methods the configuration runtime invokes on an
//! already-authorized semantic coordinator. It does not select profiles,
//! expose inventory stores, or mount a transport. Associated types keep
//! retrieval and store payloads in the crates that own them so this
//! ports-and-contracts crate does not take a `tracedecay-usecases` or
//! `tracedecay-search-eval` edge.

use std::future::Future;
use std::pin::Pin;

use thiserror::Error;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{ManifestDigest, UtcMicros};

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
    #[error("semantic activation compare-and-swap conflicted")]
    Conflict,
    #[error("semantic runtime activation failed: {0}")]
    Runtime(String),
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
