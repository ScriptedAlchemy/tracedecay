//! Immutable provider configuration admitted for one Work execution.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ConfigurationRevisionId, ConfigurationSnapshotId, CredentialReferenceId, ManifestDigest,
    UtcMicros, WorkProviderBackendV1, WorkProviderRouteV1, WorkRuntimeContractError,
    WorkTopologyPolicyV1, canonical_text,
};

const MAX_ENVIRONMENT_KEYS: usize = 128;
const MAX_CREDENTIAL_REFERENCES: usize = 64;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProviderProtocol {
    ClaudeStreamJson,
    CodexAppServerJsonRpc,
    CodexExecJson,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkSandboxPolicy {
    Required,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkApprovalPolicy {
    Never,
    OnRequest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkFilesystemPolicy {
    ReadOnly,
    WorkspaceWrite,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkEgressPolicy {
    Deny,
    Allowlisted,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExecutableReference {
    executable_id: String,
    artifact_digest: ManifestDigest,
}

impl WorkExecutableReference {
    pub fn new(
        executable_id: String,
        artifact_digest: ManifestDigest,
    ) -> Result<Self, WorkRuntimeContractError> {
        if !canonical_text::is_canonical_text_within(&executable_id, 256) {
            return Err(WorkRuntimeContractError::InvalidExecutionSnapshot);
        }
        artifact_digest
            .validate()
            .map_err(|_| WorkRuntimeContractError::InvalidExecutionSnapshot)?;
        Ok(Self {
            executable_id,
            artifact_digest,
        })
    }

    pub fn executable_id(&self) -> &str {
        &self.executable_id
    }

    pub fn artifact_digest(&self) -> &ManifestDigest {
        &self.artifact_digest
    }
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExecutionLimits {
    max_input_tokens: u64,
    max_output_tokens: u64,
    max_stdout_bytes: u64,
    max_stderr_bytes: u64,
    max_protocol_bytes: u64,
    max_concurrency: u32,
}

impl WorkExecutionLimits {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_input_tokens: u64,
        max_output_tokens: u64,
        max_stdout_bytes: u64,
        max_stderr_bytes: u64,
        max_protocol_bytes: u64,
        max_concurrency: u32,
    ) -> Result<Self, WorkRuntimeContractError> {
        if [
            max_input_tokens,
            max_output_tokens,
            max_stdout_bytes,
            max_stderr_bytes,
            max_protocol_bytes,
            u64::from(max_concurrency),
        ]
        .contains(&0)
        {
            return Err(WorkRuntimeContractError::InvalidExecutionSnapshot);
        }
        Ok(Self {
            max_input_tokens,
            max_output_tokens,
            max_stdout_bytes,
            max_stderr_bytes,
            max_protocol_bytes,
            max_concurrency,
        })
    }

    pub const fn max_input_tokens(self) -> u64 {
        self.max_input_tokens
    }

    pub const fn max_output_tokens(self) -> u64 {
        self.max_output_tokens
    }

    pub const fn max_stdout_bytes(self) -> u64 {
        self.max_stdout_bytes
    }

    pub const fn max_stderr_bytes(self) -> u64 {
        self.max_stderr_bytes
    }

    pub const fn max_protocol_bytes(self) -> u64 {
        self.max_protocol_bytes
    }

    pub const fn max_concurrency(self) -> u32 {
        self.max_concurrency
    }
}

impl<'de> Deserialize<'de> for WorkExecutionLimits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            max_input_tokens: u64,
            max_output_tokens: u64,
            max_stdout_bytes: u64,
            max_stderr_bytes: u64,
            max_protocol_bytes: u64,
            max_concurrency: u32,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.max_input_tokens,
            wire.max_output_tokens,
            wire.max_stdout_bytes,
            wire.max_stderr_bytes,
            wire.max_protocol_bytes,
            wire.max_concurrency,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkFallbackTopology {
    Disabled,
    CodexCli {
        route: WorkProviderRouteV1,
        executable: WorkExecutableReference,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkExecutionSnapshotInput {
    pub configuration_revision_id: ConfigurationRevisionId,
    pub configuration_snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: ManifestDigest,
    pub resolution_provenance_digest: ManifestDigest,
    pub route: WorkProviderRouteV1,
    pub backend: WorkProviderBackendV1,
    pub protocol: WorkProviderProtocol,
    pub model: String,
    pub executable: WorkExecutableReference,
    pub sandbox: WorkSandboxPolicy,
    pub approval: WorkApprovalPolicy,
    pub filesystem: WorkFilesystemPolicy,
    pub egress: WorkEgressPolicy,
    pub environment_allowlist: BTreeSet<String>,
    pub credential_references: BTreeSet<CredentialReferenceId>,
    pub limits: WorkExecutionLimits,
    pub deadline: UtcMicros,
    pub fallback: WorkFallbackTopology,
    /// The complete placement and Git-topology constraint admitted with this
    /// execution: sealed worktree roots, protected refs, integration mode,
    /// clean/test/review gates, retention eligibility, and notification level.
    /// The policy is carried by value so a later configuration change cannot
    /// reinterpret an active attempt, and its root locators stay sealed so no
    /// raw path reaches an execution adapter.
    pub topology: WorkTopologyPolicyV1,
}

/// Immutable provider and topology authority pinned for exactly one Work
/// execution. Every constraint the execution is governed by is named here;
/// nothing in this value is an opaque stand-in that a reader must resolve
/// against a mutable store.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExecutionSnapshot {
    configuration_revision_id: ConfigurationRevisionId,
    configuration_snapshot_id: ConfigurationSnapshotId,
    effective_behavior_digest: ManifestDigest,
    resolution_provenance_digest: ManifestDigest,
    route: WorkProviderRouteV1,
    backend: WorkProviderBackendV1,
    protocol: WorkProviderProtocol,
    model: String,
    executable: WorkExecutableReference,
    sandbox: WorkSandboxPolicy,
    approval: WorkApprovalPolicy,
    filesystem: WorkFilesystemPolicy,
    egress: WorkEgressPolicy,
    environment_allowlist: BTreeSet<String>,
    credential_references: BTreeSet<CredentialReferenceId>,
    limits: WorkExecutionLimits,
    deadline: UtcMicros,
    fallback: WorkFallbackTopology,
    /// See [`WorkExecutionSnapshotInput::topology`]. Validated on construction,
    /// so an admitted snapshot can never pin a topology that weakens the
    /// protected-ref floor or permits a native integration without its gates.
    topology: WorkTopologyPolicyV1,
}

impl WorkExecutionSnapshot {
    pub fn new(input: WorkExecutionSnapshotInput) -> Result<Self, WorkRuntimeContractError> {
        let snapshot = Self {
            configuration_revision_id: input.configuration_revision_id,
            configuration_snapshot_id: input.configuration_snapshot_id,
            effective_behavior_digest: input.effective_behavior_digest,
            resolution_provenance_digest: input.resolution_provenance_digest,
            route: input.route,
            backend: input.backend,
            protocol: input.protocol,
            model: input.model,
            executable: input.executable,
            sandbox: input.sandbox,
            approval: input.approval,
            filesystem: input.filesystem,
            egress: input.egress,
            environment_allowlist: input.environment_allowlist,
            credential_references: input.credential_references,
            limits: input.limits,
            deadline: input.deadline,
            fallback: input.fallback,
            topology: input.topology,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    fn validate(&self) -> Result<(), WorkRuntimeContractError> {
        self.configuration_revision_id
            .validate()
            .and_then(|_| self.configuration_snapshot_id.validate())
            .and_then(|_| self.effective_behavior_digest.validate())
            .and_then(|_| self.resolution_provenance_digest.validate())
            .and_then(|_| self.topology.validate())
            .map_err(|_| WorkRuntimeContractError::InvalidExecutionSnapshot)?;
        if !canonical_text::is_canonical_text_within(&self.model, 256)
            || self.deadline.0 <= 0
            || self.route.provider_id() != self.backend.provider_id()
            || self.protocol != self.backend.protocol()
            || self.environment_allowlist.len() > MAX_ENVIRONMENT_KEYS
            || self.credential_references.len() > MAX_CREDENTIAL_REFERENCES
            || self.environment_allowlist.iter().any(|key| {
                key.len() > 128
                    || key.is_empty()
                    || !key.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        {
            return Err(WorkRuntimeContractError::InvalidExecutionSnapshot);
        }
        match &self.fallback {
            WorkFallbackTopology::Disabled => {}
            WorkFallbackTopology::CodexCli { route, .. }
                if route.provider_id() == WorkProviderBackendV1::CodexCli.provider_id()
                    && self.backend != WorkProviderBackendV1::CodexCli => {}
            WorkFallbackTopology::CodexCli { .. } => {
                return Err(WorkRuntimeContractError::InvalidExecutionSnapshot);
            }
        }
        Ok(())
    }

    pub fn configuration_revision_id(&self) -> &ConfigurationRevisionId {
        &self.configuration_revision_id
    }

    pub fn configuration_snapshot_id(&self) -> &ConfigurationSnapshotId {
        &self.configuration_snapshot_id
    }

    pub fn effective_behavior_digest(&self) -> &ManifestDigest {
        &self.effective_behavior_digest
    }

    pub fn resolution_provenance_digest(&self) -> &ManifestDigest {
        &self.resolution_provenance_digest
    }

    pub fn route(&self) -> &WorkProviderRouteV1 {
        &self.route
    }

    pub const fn backend(&self) -> WorkProviderBackendV1 {
        self.backend
    }

    pub const fn protocol(&self) -> WorkProviderProtocol {
        self.protocol
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn executable(&self) -> &WorkExecutableReference {
        &self.executable
    }

    pub const fn sandbox(&self) -> WorkSandboxPolicy {
        self.sandbox
    }

    pub const fn approval(&self) -> WorkApprovalPolicy {
        self.approval
    }

    pub const fn filesystem(&self) -> WorkFilesystemPolicy {
        self.filesystem
    }

    pub const fn egress(&self) -> WorkEgressPolicy {
        self.egress
    }

    pub fn environment_allowlist(&self) -> &BTreeSet<String> {
        &self.environment_allowlist
    }

    pub fn credential_references(&self) -> &BTreeSet<CredentialReferenceId> {
        &self.credential_references
    }

    pub const fn limits(&self) -> WorkExecutionLimits {
        self.limits
    }

    pub const fn deadline(&self) -> UtcMicros {
        self.deadline
    }

    pub fn fallback(&self) -> &WorkFallbackTopology {
        &self.fallback
    }

    /// The pinned placement and Git-topology constraint. Callers that need the
    /// frozen digest derive it with [`WorkTopologyPolicyV1::compute_digest`]
    /// rather than trusting a separately supplied one.
    pub fn topology(&self) -> &WorkTopologyPolicyV1 {
        &self.topology
    }
}

impl<'de> Deserialize<'de> for WorkExecutionSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = WorkExecutionSnapshotInputWire::deserialize(deserializer)?;
        Self::new(input.into()).map_err(serde::de::Error::custom)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkExecutionSnapshotInputWire {
    configuration_revision_id: ConfigurationRevisionId,
    configuration_snapshot_id: ConfigurationSnapshotId,
    effective_behavior_digest: ManifestDigest,
    resolution_provenance_digest: ManifestDigest,
    route: WorkProviderRouteV1,
    backend: WorkProviderBackendV1,
    protocol: WorkProviderProtocol,
    model: String,
    executable: WorkExecutableReference,
    sandbox: WorkSandboxPolicy,
    approval: WorkApprovalPolicy,
    filesystem: WorkFilesystemPolicy,
    egress: WorkEgressPolicy,
    environment_allowlist: BTreeSet<String>,
    credential_references: BTreeSet<CredentialReferenceId>,
    limits: WorkExecutionLimits,
    deadline: UtcMicros,
    fallback: WorkFallbackTopology,
    topology: WorkTopologyPolicyV1,
}

impl From<WorkExecutionSnapshotInputWire> for WorkExecutionSnapshotInput {
    fn from(wire: WorkExecutionSnapshotInputWire) -> Self {
        Self {
            configuration_revision_id: wire.configuration_revision_id,
            configuration_snapshot_id: wire.configuration_snapshot_id,
            effective_behavior_digest: wire.effective_behavior_digest,
            resolution_provenance_digest: wire.resolution_provenance_digest,
            route: wire.route,
            backend: wire.backend,
            protocol: wire.protocol,
            model: wire.model,
            executable: wire.executable,
            sandbox: wire.sandbox,
            approval: wire.approval,
            filesystem: wire.filesystem,
            egress: wire.egress,
            environment_allowlist: wire.environment_allowlist,
            credential_references: wire.credential_references,
            limits: wire.limits,
            deadline: wire.deadline,
            fallback: wire.fallback,
            topology: wire.topology,
        }
    }
}
