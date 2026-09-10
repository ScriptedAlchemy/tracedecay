//! Canonical provider-executable bindings carried by effective configuration.

use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    DomainError, ProviderId, WorkExecutableReference, WorkProviderBackendV1, WorkProviderProtocol,
    WorkRouteCandidateV1, canonical_text,
};

/// One executable capability admitted by a configured artifact binding.
///
/// The closed variants bind a provider backend to its exact wire protocol.
/// Callers cannot claim an arbitrary backend/protocol combination.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkExecutableCapabilityV1 {
    ClaudeCodeStreamJson,
    CodexAppServerJsonRpc,
    CodexCliExecJson,
}

impl WorkExecutableCapabilityV1 {
    pub const fn admits(
        self,
        backend: WorkProviderBackendV1,
        protocol: WorkProviderProtocol,
    ) -> bool {
        matches!(
            (self, backend, protocol),
            (
                Self::ClaudeCodeStreamJson,
                WorkProviderBackendV1::ClaudeCodeCli,
                WorkProviderProtocol::ClaudeStreamJson,
            ) | (
                Self::CodexAppServerJsonRpc,
                WorkProviderBackendV1::CodexAppServer,
                WorkProviderProtocol::CodexAppServerJsonRpc,
            ) | (
                Self::CodexCliExecJson,
                WorkProviderBackendV1::CodexCli,
                WorkProviderProtocol::CodexExecJson,
            )
        )
    }

    pub const fn backend(self) -> WorkProviderBackendV1 {
        match self {
            Self::ClaudeCodeStreamJson => WorkProviderBackendV1::ClaudeCodeCli,
            Self::CodexAppServerJsonRpc => WorkProviderBackendV1::CodexAppServer,
            Self::CodexCliExecJson => WorkProviderBackendV1::CodexCli,
        }
    }

    pub const fn protocol(self) -> WorkProviderProtocol {
        self.backend().protocol()
    }

    pub fn provider_id(self) -> &'static ProviderId {
        self.backend().provider_id()
    }
}

/// Exact on-disk executable selected for one opaque executable identity.
///
/// The path is configuration data, never a lookup hint. Runtime admission
/// canonicalizes it again and verifies the file bytes against
/// `executable.artifact_digest` before returning an executable binding.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExecutableBindingV1 {
    executable: WorkExecutableReference,
    canonical_path: PathBuf,
    capabilities: Vec<WorkExecutableCapabilityV1>,
    routes: Vec<WorkRouteCandidateV1>,
}

impl WorkExecutableBindingV1 {
    pub fn new(
        executable: WorkExecutableReference,
        canonical_path: PathBuf,
        capabilities: Vec<WorkExecutableCapabilityV1>,
        routes: Vec<WorkRouteCandidateV1>,
    ) -> Result<Self, DomainError> {
        let binding = Self {
            executable,
            canonical_path,
            capabilities,
            routes,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn executable(&self) -> &WorkExecutableReference {
        &self.executable
    }

    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn capabilities(&self) -> &[WorkExecutableCapabilityV1] {
        &self.capabilities
    }

    pub fn routes(&self) -> &[WorkRouteCandidateV1] {
        &self.routes
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if !canonical_text::is_canonical_text_within(self.executable.executable_id(), 256)
            || self.executable.artifact_digest().validate().is_err()
            || !self.canonical_path.is_absolute()
            || self
                .canonical_path
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(DomainError::NonCanonical {
                field: "work executable canonical path",
            });
        }
        if self.capabilities.is_empty()
            || self.capabilities.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(DomainError::NonCanonical {
                field: "work executable capabilities",
            });
        }
        if self
            .routes
            .windows(2)
            .any(|pair| pair[0].route_id >= pair[1].route_id)
        {
            return Err(DomainError::NonCanonical {
                field: "work executable routes",
            });
        }
        for route in &self.routes {
            route.validate()?;
            if !self
                .capabilities
                .iter()
                .any(|capability| capability.provider_id().as_str() == route.provider_capability_id)
            {
                return Err(DomainError::NonCanonical {
                    field: "work executable route provider",
                });
            }
        }
        Ok(())
    }
}

/// Validates the complete executable-id mapping stored in one setting value.
pub(crate) fn validate_work_executable_bindings(
    bindings: &[WorkExecutableBindingV1],
) -> Result<(), DomainError> {
    if bindings
        .windows(2)
        .any(|pair| pair[0].executable().executable_id() >= pair[1].executable().executable_id())
    {
        return Err(DomainError::NonCanonical {
            field: "work executable binding order",
        });
    }
    for binding in bindings {
        binding.validate()?;
    }
    let mut route_ids = std::collections::BTreeSet::new();
    if bindings
        .iter()
        .flat_map(WorkExecutableBindingV1::routes)
        .any(|route| !route_ids.insert(route.route_id.as_str()))
    {
        return Err(DomainError::NonCanonical {
            field: "work executable route identities",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ManifestDigest, WorkContentLocationClassV1, WorkEffortClassV1, WorkOrdinalBandV1};

    fn reference(id: &str, byte: char) -> WorkExecutableReference {
        WorkExecutableReference::new(
            id.to_owned(),
            ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap(),
        )
        .unwrap()
    }

    fn absolute(name: &str) -> PathBuf {
        std::env::current_dir().unwrap().join(name)
    }

    fn route(route_id: &str, provider_id: &str) -> WorkRouteCandidateV1 {
        WorkRouteCandidateV1 {
            route_id: route_id.to_owned(),
            provider_capability_id: provider_id.to_owned(),
            model_id: "model.work-route".to_owned(),
            effort: WorkEffortClassV1::Standard,
            declared_budget_ceiling: 1,
            content_location: WorkContentLocationClassV1::Local,
            correctness: WorkOrdinalBandV1::High,
            sensitive_data_fitness: WorkOrdinalBandV1::High,
            latency: WorkOrdinalBandV1::Moderate,
            cost: WorkOrdinalBandV1::Moderate,
            autonomy: WorkOrdinalBandV1::High,
            evidence_quality: WorkOrdinalBandV1::High,
        }
    }

    #[test]
    fn executable_binding_requires_absolute_clean_path_and_sorted_capabilities() {
        assert!(
            WorkExecutableBindingV1::new(
                reference("codex", '1'),
                PathBuf::from("bin/codex"),
                vec![WorkExecutableCapabilityV1::CodexAppServerJsonRpc],
                Vec::new(),
            )
            .is_err()
        );
        assert!(
            WorkExecutableBindingV1::new(
                reference("codex", '1'),
                absolute("opt").join("..").join("bin").join("codex"),
                vec![WorkExecutableCapabilityV1::CodexAppServerJsonRpc],
                Vec::new(),
            )
            .is_err()
        );
        assert!(
            WorkExecutableBindingV1::new(
                reference("codex", '1'),
                absolute("codex"),
                vec![
                    WorkExecutableCapabilityV1::CodexCliExecJson,
                    WorkExecutableCapabilityV1::CodexAppServerJsonRpc,
                ],
                Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn executable_binding_map_rejects_duplicate_ids() {
        let first = WorkExecutableBindingV1::new(
            reference("codex", '1'),
            absolute("codex-one"),
            vec![WorkExecutableCapabilityV1::CodexAppServerJsonRpc],
            Vec::new(),
        )
        .unwrap();
        let second = WorkExecutableBindingV1::new(
            reference("codex", '2'),
            absolute("codex-two"),
            vec![WorkExecutableCapabilityV1::CodexCliExecJson],
            Vec::new(),
        )
        .unwrap();

        assert!(validate_work_executable_bindings(&[first, second]).is_err());
    }

    #[test]
    fn executable_routes_require_matching_capabilities_and_unique_identities() {
        assert!(
            WorkExecutableBindingV1::new(
                reference("codex", '1'),
                absolute("codex"),
                vec![WorkExecutableCapabilityV1::CodexAppServerJsonRpc],
                vec![route(
                    "route.work.codex-mismatch",
                    "provider.work.claude-code-cli",
                )],
            )
            .is_err()
        );

        let first = WorkExecutableBindingV1::new(
            reference("codex-one", '1'),
            absolute("codex-one"),
            vec![WorkExecutableCapabilityV1::CodexAppServerJsonRpc],
            vec![route(
                "route.work.duplicate",
                "provider.work.codex-app-server",
            )],
        )
        .unwrap();
        let second = WorkExecutableBindingV1::new(
            reference("codex-two", '2'),
            absolute("codex-two"),
            vec![WorkExecutableCapabilityV1::CodexAppServerJsonRpc],
            vec![route(
                "route.work.duplicate",
                "provider.work.codex-app-server",
            )],
        )
        .unwrap();

        assert!(validate_work_executable_bindings(&[first, second]).is_err());
    }
}
