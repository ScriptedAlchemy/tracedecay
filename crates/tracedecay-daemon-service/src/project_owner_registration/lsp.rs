//! Project-open LSP registration adapter.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_contracts::ApplicationContractError;
use tracedecay_domain::{UtcMicros, canonical_sha256};
use tracedecay_lsp::analyzer::broker::AdmittedLspProvider;
use tracedecay_lsp::{
    ContextProjectionKind, GatewayCapabilities, SemanticCapability, TRACEDECAY_CONTEXT_REVISION,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const POLICY_REVISION_V1: u64 = 1;

pub fn project_open_lsp_scope_grant(
    access: &ProjectSourceAccessSnapshot,
    observed_at: UtcMicros,
) -> Result<tracedecay_contracts::CapabilityGrantSnapshot, ApplicationContractError> {
    let capability = CapabilityId::new(crate::LSP_WORKSPACE_CAPABILITY_ID_V1).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open LSP workspace capability",
        }
    })?;
    if observed_at >= access.grant_expires_at
        || !access.effective_capabilities.contains(&capability)
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open LSP workspace capability grant",
        });
    }
    let use_case = UseCaseId::new(crate::LSP_WORKSPACE_USE_CASE_ID_V1).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open LSP workspace use case",
        }
    })?;
    let capabilities = BTreeSet::from([capability]);
    let use_cases = BTreeSet::from([use_case]);
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.lsp-workspace-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
        &capabilities,
        &use_cases,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open LSP workspace grant digest",
    })?;
    tracedecay_contracts::CapabilityGrantSnapshot::new(
        tracedecay_contracts::CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.project-open.lsp-workspace.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        access.grant_expires_at,
        access.scope.clone(),
        capabilities,
        use_cases,
        tracedecay_contracts::DisclosureClass::Sensitive,
    )
}

pub fn production_lsp_registration(
    admitted_providers: &[AdmittedLspProvider],
) -> (Vec<String>, GatewayCapabilities) {
    let revision = TRACEDECAY_CONTEXT_REVISION;
    let gateway_capabilities = GatewayCapabilities {
        supports_publish_diagnostics: true,
        supports_document_diagnostics: true,
        supports_workspace_diagnostics: true,
        supports_managed_diagnostics: true,
        // Multi-root admission is enabled only after the registrar mounts the
        // exact authorized scope-set storage.
        supports_workspace_folders: false,
        // The gateway implements the complete request protocol. The upstream
        // initialize response remains the authority for which methods can be
        // advertised to a particular session.
        semantic: SemanticCapability::ALL.into_iter().collect(),
        context_projections: BTreeMap::from([
            (ContextProjectionKind::diagnostics(), revision),
            (ContextProjectionKind::post_edit_impact(), revision),
            (ContextProjectionKind::affected_tests(), revision),
            (ContextProjectionKind::test_run_results(), revision),
        ]),
        supports_context_expansion: true,
    };
    (
        admitted_providers
            .iter()
            .map(|provider| provider.language.clone())
            .collect(),
        gateway_capabilities,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_application::source_authorization::ProjectSourceAccessSnapshot;
    use tracedecay_domain::configuration::{
        AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceBindingId, SourceKindV1,
    };
    use tracedecay_domain::{
        ActorId, LocatorDigest, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_lsp::SemanticCapability;
    use tracedecay_lsp::analyzer::broker::AdmittedLspProvider;
    use tracedecay_tool_catalog::CapabilityId;

    use super::{production_lsp_registration, project_open_lsp_scope_grant};

    #[test]
    fn lsp_scope_grant_requires_live_effective_capability() {
        let mut access = source_access();
        project_open_lsp_scope_grant(&access, UtcMicros(10)).expect("live LSP grant");

        access.effective_capabilities.clear();
        assert!(project_open_lsp_scope_grant(&access, UtcMicros(10)).is_err());

        access.effective_capabilities.insert(
            CapabilityId::new(crate::LSP_WORKSPACE_CAPABILITY_ID_V1).expect("LSP capability"),
        );
        assert!(project_open_lsp_scope_grant(&access, access.grant_expires_at).is_err());
    }

    fn source_access() -> ProjectSourceAccessSnapshot {
        let project_id = ProjectId::new("project.lsp-owner").expect("project id");
        ProjectSourceAccessSnapshot {
            scope: tracedecay_contracts::ResolvedScope::new(
                project_id.clone(),
                RepositoryId::new("repository.lsp-owner").expect("repository id"),
                WorktreeId::new("worktree.lsp-owner").expect("worktree id"),
                None,
            )
            .expect("scope"),
            requester: ActorId::new("actor.lsp-owner").expect("requester"),
            binding: ScopeSourceBinding::new(
                SourceBindingId::new("binding.lsp-owner").expect("binding id"),
                SourceKindV1::Cursor,
                LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).expect("locator digest"),
                AuthorityRef::Project(project_id),
            )
            .expect("binding"),
            configuration_revision: ConfigurationRevisionId::new("revision.lsp-owner")
                .expect("configuration revision"),
            configuration_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("configuration digest"),
            configuration_provenance_digest: ManifestDigest::new(format!(
                "sha256:{}",
                "c".repeat(64)
            ))
            .expect("configuration provenance"),
            effective_capabilities: BTreeSet::from([CapabilityId::new(
                crate::LSP_WORKSPACE_CAPABILITY_ID_V1,
            )
            .expect("LSP capability")]),
            grant_expires_at: UtcMicros(100),
        }
    }

    fn admitted(language: &str, analyzer_available: bool) -> AdmittedLspProvider {
        AdmittedLspProvider {
            language: language.to_owned(),
            command: format!("{language}-language-server"),
            analyzer_available,
        }
    }

    #[test]
    fn registration_mounts_dynamic_workspace_diagnostics_without_analyzer() {
        let (languages, gateway) = production_lsp_registration(&[admitted("rust", false)]);

        assert_eq!(languages, vec!["rust"]);
        assert!(gateway.supports_document_diagnostics);
        assert!(gateway.supports_managed_diagnostics);
        assert!(gateway.supports_workspace_diagnostics);
        assert_eq!(
            gateway.semantic,
            SemanticCapability::ALL.into_iter().collect()
        );
    }

    #[test]
    fn registration_preserves_every_admitted_project_language() {
        for language in ["python", "typescript"] {
            let admitted = [
                admitted("rust", false),
                admitted(language, true),
                admitted("go", false),
            ];
            let (selected, gateway) = production_lsp_registration(&admitted);

            assert_eq!(selected, vec!["rust", language, "go"]);
            assert_eq!(
                gateway.semantic,
                SemanticCapability::ALL.into_iter().collect()
            );
        }
    }
}
