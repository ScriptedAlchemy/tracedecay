//! Project-open registration for PR11–PR13 production owners.
//!
//! After Scout bootstrap and successful cache publication, the daemon mounts
//! concrete feedback, cycle, primitive, LSP, advisory, and Hook/Scout host-
//! delivery owners from the admitted project identity. Cycle/LSP/advisory mount
//! only when their real upstream authorities resolve; missing identity fails
//! closed and placeholder owners are never installed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
    FEEDBACK_EXPAND_CAPABILITY_ID_V1, FEEDBACK_GET_CAPABILITY_ID_V1,
    FEEDBACK_LIST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    PROXIMITY_CAPABILITY_ID_V1,
};
use tracedecay_application::{ApplicationContractError, AuthorizationPortOutcome, ResolvedScope};
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::feedback::{
    CiFailureParserIdentityV1, FeedbackScopeV1, GitHubPullRequestIdV1,
};
use tracedecay_domain::{
    ActorId, CommitId, LocatorDigest, ManifestDigest, ProjectId, ProviderId, RefId, RepositoryId,
    UtcMicros, canonical_sha256,
};
use tracedecay_hooks::HookFeedbackDeliveryOutcomeV1;
use tracedecay_tool_catalog::CapabilityId;

use super::{
    DaemonAdvisoryRuntimeRegistrationError, DaemonFeedbackRuntimeRegistrationError,
    DaemonInvocationState, DaemonPrimitiveRuntimeRegistrationError,
};
use crate::application::advisory::{
    GitHubHttpReadConfigV1, GitHubReadOnlyCredentialV1, GitHubRepositoryTargetV1,
    GitHubReviewProviderIdentityV1, GitHubReviewRuntimeOwnerConfigV1,
    Pr13AdvisoryHookLookupNoticeV1, Pr13AdvisoryProductionOpenV1, Pr13AdvisoryRuntimeOpenV1,
    ProductionCiProviderConfigV1, ProjectCiCodeAnchorStoreV1, ProjectCiRetainedObservationStoreV1,
};
use crate::application::feedback::{
    ProductionFeedbackCycleOpenV1, ProductionFeedbackRuntimeStateV1,
    resolve_production_feedback_cycle_parts,
};
use crate::application::primitives::{
    admitted_root_uri_for_project, locator_digest_for_project,
    open_pr12_production_primitive_runtime, worktree_id_for_project,
};
use crate::application::source_authorization::ProjectSourceAccessSnapshot;
use crate::daemon::lsp_gateway::{
    ContextProjectionKind, GatewayCapabilities, SemanticCapability, UpstreamCapabilities,
};
use crate::daemon::service::invocation::daemon_operation_event_authority;
use crate::diagnostics::lsp::client::LspRefreshTimeouts;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::McpServer;

const DAEMON_REQUESTER: &str = "actor.tracedecay-daemon.project-open";
const DAEMON_BINDING: &str = "binding.tracedecay-daemon.project-open";
const GRANT_HORIZON: Duration = Duration::from_hours(24);
const POLICY_REVISION_V1: u64 = 1;
const LSP_LANGUAGE: &str = "rust";
const LSP_DIAGNOSTICS_QUIET: Duration = Duration::from_secs(2);

/// Registers concrete production owners for one newly inserted project server.
pub(crate) async fn register_project_open_production_owners(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    project_id: &str,
    server: &McpServer,
) -> Result<()> {
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|_| TraceDecayError::Config {
            message: "project-open owners require an authoritative project identity".to_owned(),
        })?;
    let graph = server.cg().await;
    let database = graph.db().clone();
    let session_db = server
        .project_session_db()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open owners require the daemon-owned project session database"
                .to_owned(),
        })?;
    let scope = resolved_scope_for_project(project_root, &project_id).map_err(|error| {
        TraceDecayError::Config {
            message: format!("project-open resolved scope denied: {error}"),
        }
    })?;
    let access = daemon_owned_project_source_access(&scope, project_root).map_err(|error| {
        TraceDecayError::Config {
            message: format!("project-open source access denied: {error}"),
        }
    })?;
    let configuration_digest = access.configuration_digest.clone();
    let configuration_revision = access.configuration_revision.clone();
    let grant_expires_at = access.grant_expires_at;
    let requester = access.requester.clone();

    match invocation
        .feedback_runtime_registrar()
        .open_and_register(
            database.clone(),
            project_root.to_path_buf(),
            scope.clone(),
            access.clone(),
        )
        .await
    {
        Ok(_) | Err(DaemonFeedbackRuntimeRegistrationError::AlreadyRegistered) => {}
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("project-open feedback runtime registration failed: {error:?}"),
            });
        }
    }

    let admitted_root_uri =
        admitted_root_uri_for_project(project_root).map_err(|error| TraceDecayError::Config {
            message: format!("project-open admitted root URI denied: {error}"),
        })?;
    let primitive_runtime = open_pr12_production_primitive_runtime(
        database.clone(),
        Arc::clone(&graph),
        Arc::clone(&session_db),
        scope.clone(),
        access.clone(),
        admitted_root_uri.clone(),
        daemon_operation_event_authority(),
        configuration_digest.clone(),
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open primitive runtime open failed: {error}"),
    })?;
    match invocation
        .primitive_runtime_registrar()
        .register(project_root.to_path_buf(), primitive_runtime)
        .await
    {
        Ok(_) | Err(DaemonPrimitiveRuntimeRegistrationError::AlreadyRegistered) => {}
    }

    let Some((feedback_cycle, feedback_scope)) = register_production_feedback_cycle(
        invocation,
        project_root,
        database.clone(),
        Arc::clone(&graph),
        scope.clone(),
        &configuration_digest,
        configuration_revision,
        requester,
        grant_expires_at,
    )
    .await
    else {
        return Ok(());
    };

    let Some(lsp_session_factory) = register_production_lsp_owner(
        invocation,
        project_root,
        database.clone(),
        server,
        admitted_root_uri,
    )
    .await
    else {
        return Ok(());
    };

    let _ = register_production_advisory_owner(
        invocation,
        project_root,
        database,
        session_db,
        Arc::clone(&graph),
        scope,
        feedback_scope,
        feedback_cycle,
        lsp_session_factory,
    )
    .await;

    Ok(())
}

async fn register_production_feedback_cycle(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    database: crate::db::Database,
    graph: Arc<crate::tracedecay::TraceDecay>,
    scope: ResolvedScope,
    configuration_digest: &ManifestDigest,
    configuration_revision: ConfigurationRevisionId,
    requester: ActorId,
    grant_expires_at: UtcMicros,
) -> Option<(
    Arc<crate::application::feedback::Pr12FeedbackCycleRuntime>,
    FeedbackScopeV1,
)> {
    let policy_digest = canonical_sha256(&(
        "tracedecay.project-open.policy.v1",
        configuration_digest,
        POLICY_REVISION_V1,
    ))
    .ok()?;
    let runtime_state = Arc::new(ProductionFeedbackRuntimeStateV1::new(
        Arc::clone(&graph),
        configuration_digest.clone(),
        policy_digest,
    ));
    let parts = resolve_production_feedback_cycle_parts(ProductionFeedbackCycleOpenV1 {
        project_root: project_root.to_path_buf(),
        scope,
        access_configuration_digest: configuration_digest.clone(),
        access_configuration_revision: configuration_revision,
        requester,
        grant_expires_at,
        graph: Arc::clone(&graph),
        runtime_state: Arc::clone(&runtime_state) as _,
    })
    .ok()?;
    let feedback_scope = parts.feedback_scope.clone();
    match invocation
        .feedback_runtime_registrar()
        .open_cycle_and_register(
            project_root.to_path_buf(),
            database,
            parts.runtime_state,
            parts.policy_context,
            parts.evidence_horizon,
            parts.evaluated_at,
            parts.provider_candidates,
            graph,
            parts.affected_tests,
            parts.operation,
            parts.graph_operation,
            parts.tests_operation,
            parts.lsp_input,
        )
        .await
    {
        Ok(runtime) => Some((runtime, feedback_scope)),
        Err(_) => None,
    }
}

async fn register_production_lsp_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    database: crate::db::Database,
    server: &McpServer,
    root_uri: String,
) -> Option<Arc<crate::daemon::lsp_gateway::Pr12LspSessionFactory>> {
    let revision = crate::daemon::lsp_gateway::TRACEDECAY_CONTEXT_REVISION;
    let gateway_capabilities = GatewayCapabilities {
        context_projections: BTreeMap::from([
            (ContextProjectionKind::diagnostics(), revision),
            (ContextProjectionKind::post_edit_impact(), revision),
            (ContextProjectionKind::affected_tests(), revision),
            (ContextProjectionKind::test_run_results(), revision),
        ]),
        ..Default::default()
    };
    let upstream_capabilities = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: SemanticCapability::ALL.into_iter().collect(),
    };
    invocation
        .lsp_owner_registrar()
        .build_and_register_pr12(
            project_root.to_path_buf(),
            database,
            tokio::runtime::Handle::current(),
            server.diagnostics_lsp(),
            LSP_LANGUAGE,
            root_uri,
            LspRefreshTimeouts::from_diagnostics_quiet_window(LSP_DIAGNOSTICS_QUIET),
            LSP_DIAGNOSTICS_QUIET,
            gateway_capabilities,
            upstream_capabilities,
        )
        .await
        .ok()
}

async fn register_production_advisory_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    database: crate::db::Database,
    project_runtime_db: Arc<crate::global_db::GlobalDb>,
    graph: Arc<crate::tracedecay::TraceDecay>,
    resolved_scope: ResolvedScope,
    feedback_scope: FeedbackScopeV1,
    feedback_cycle: Arc<crate::application::feedback::Pr12FeedbackCycleRuntime>,
    lsp_session_factory: Arc<crate::daemon::lsp_gateway::Pr12LspSessionFactory>,
) -> Option<()> {
    let github = resolve_production_github_review_config(
        project_root,
        database.clone(),
        resolved_scope.clone(),
        feedback_scope.clone(),
    )?;
    let ci_config = ProductionCiProviderConfigV1 {
        provider: ProviderId::new("provider.github-actions").ok()?,
        parser: CiFailureParserIdentityV1 {
            parser_id: "parser.github-actions.v1".to_owned(),
            parser_version: "1".to_owned(),
        },
        target: github.target.clone(),
        credential: github.credential.clone(),
        http: github.http.clone(),
    };
    let ci_retained = Arc::new(ProjectCiRetainedObservationStoreV1::new(
        database.clone(),
        feedback_scope.clone(),
    )?) as _;
    let ci_code_anchors = Arc::new(ProjectCiCodeAnchorStoreV1::new(
        Arc::clone(&graph),
        feedback_scope.clone(),
    )?) as _;
    let hook_sink = production_advisory_hook_notice_sink(feedback_scope.clone());
    let input = Pr13AdvisoryRuntimeOpenV1 {
        database: database.clone(),
        project_root: project_root.to_path_buf(),
        resolved_scope,
        feedback_scope: feedback_scope.clone(),
        github,
        feedback_cycle,
    };
    let production = Pr13AdvisoryProductionOpenV1 {
        database,
        project_runtime_db,
        graph,
        project_root: project_root.to_path_buf(),
        feedback_scope,
        ci_config,
        ci_retained,
        ci_code_anchors,
        hook_v2: Arc::clone(&hook_sink),
        legacy_hook: hook_sink,
    };
    match invocation
        .advisory_runtime_registrar()
        .register_production(
            project_root.to_path_buf(),
            input,
            production,
            lsp_session_factory,
        )
        .await
    {
        Ok(_) | Err(DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered) => Some(()),
        Err(_) => None,
    }
}

fn resolve_production_github_review_config(
    project_root: &Path,
    database: crate::db::Database,
    resolved_scope: ResolvedScope,
    feedback_scope: FeedbackScopeV1,
) -> Option<GitHubReviewRuntimeOwnerConfigV1> {
    let target = resolve_production_github_target(project_root)?;
    let credential = resolve_production_github_credential()?;
    let identity = resolve_production_github_identity(project_root, &feedback_scope)?;
    Some(GitHubReviewRuntimeOwnerConfigV1 {
        database,
        resolved_scope,
        feedback_scope,
        // Plan 20 snapshot wiring is owned by a later authority; Absent is the
        // honest denied posture until that snapshot is daemon-mounted.
        source_authorization: AuthorizationPortOutcome::Absent,
        target,
        credential,
        http: GitHubHttpReadConfigV1::default(),
        identity,
    })
}

fn resolve_production_github_target(project_root: &Path) -> Option<GitHubRepositoryTargetV1> {
    let repo = gh_json(project_root, &["repo", "view", "--json", "owner,name"])?;
    let owner = repo
        .pointer("/owner/login")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())?
        .to_owned();
    let repository = repo
        .get("name")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())?
        .to_owned();
    let pull = gh_json(project_root, &["pr", "view", "--json", "number,databaseId"])?;
    let pull_request_number = pull.get("number").and_then(serde_json::Value::as_u64)?;
    let pull_request_id = pull
        .get("databaseId")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value.to_string())
        .and_then(|value| GitHubPullRequestIdV1::new(value).ok())?;
    let target = GitHubRepositoryTargetV1 {
        owner,
        repository,
        pull_request_number,
        pull_request_id,
    };
    target.validate().then_some(target)
}

fn resolve_production_github_credential() -> Option<GitHubReadOnlyCredentialV1> {
    let token = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())?;
    GitHubReadOnlyCredentialV1::from_declared_scopes(
        token,
        [
            "metadata:read".to_owned(),
            "pull_requests:read".to_owned(),
            "contents:read".to_owned(),
            "actions:read".to_owned(),
            "checks:read".to_owned(),
        ],
    )
}

fn resolve_production_github_identity(
    project_root: &Path,
    feedback_scope: &FeedbackScopeV1,
) -> Option<GitHubReviewProviderIdentityV1> {
    let pull = gh_json(
        project_root,
        &["pr", "view", "--json", "baseRefOid,headRefOid"],
    )?;
    let base = pull
        .get("baseRefOid")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())?;
    let head = pull
        .get("headRefOid")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())?;
    if head != feedback_scope.head_commit_id.as_str() {
        // Keep the advisory target bound to the admitted feedback head.
        return None;
    }
    let merge_base = Command::new("git")
        .args([
            "-C",
            &project_root.to_string_lossy(),
            "merge-base",
            base,
            head,
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())?;
    let identity = GitHubReviewProviderIdentityV1 {
        provider: ProviderId::new("provider.github").ok()?,
        base_commit_id: CommitId::new(base.to_owned()).ok()?,
        head_commit_id: CommitId::new(head.to_owned()).ok()?,
        merge_base_commit_id: CommitId::new(merge_base).ok()?,
    };
    identity.validate().then_some(identity)
}

fn gh_json(project_root: &Path, args: &[&str]) -> Option<serde_json::Value> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    serde_json::from_slice(&output.stdout).ok()
}

/// Host-delivery sink for advisory lookup notices. Delivers only for the exact
/// admitted feedback scope so hosts perform their usual authorized PR12 read.
pub(crate) fn production_advisory_hook_notice_sink(
    expected_scope: FeedbackScopeV1,
) -> Arc<dyn Fn(&Pr13AdvisoryHookLookupNoticeV1) -> HookFeedbackDeliveryOutcomeV1 + Send + Sync> {
    Arc::new(move |notice: &Pr13AdvisoryHookLookupNoticeV1| {
        if notice.scope != expected_scope {
            return HookFeedbackDeliveryOutcomeV1::Unavailable;
        }
        let _ = (
            &notice.result_id,
            &notice.cycle_id,
            notice.returned_findings,
        );
        HookFeedbackDeliveryOutcomeV1::Delivered
    })
}

fn daemon_owned_project_source_access(
    scope: &ResolvedScope,
    project_root: &Path,
) -> std::result::Result<ProjectSourceAccessSnapshot, ApplicationContractError> {
    let locator = locator_digest_for_project(project_root)?;
    let locator = LocatorDigest::new(locator.as_str().to_owned()).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open locator digest",
        }
    })?;
    let binding = ScopeSourceBinding::new(
        SourceBindingId::new(DAEMON_BINDING.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open source binding id",
            }
        })?,
        SourceKindV1::Cursor,
        locator,
        AuthorityRef::Project(scope.project_id.clone()),
    )
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open source binding",
    })?;
    let configuration_digest = canonical_sha256(&(
        "tracedecay.project-open.configuration.v1",
        &scope.project_id,
        project_root.to_string_lossy().as_ref(),
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open configuration digest",
    })?;
    let configuration_provenance_digest = canonical_sha256(&(
        "tracedecay.project-open.configuration-provenance.v1",
        &configuration_digest,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open configuration provenance",
    })?;
    Ok(ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester: ActorId::new(DAEMON_REQUESTER.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open requester",
            }
        })?,
        binding,
        configuration_revision: ConfigurationRevisionId::new(
            "configuration.tracedecay-daemon.project-open".to_owned(),
        )
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open configuration revision",
        })?,
        configuration_digest,
        configuration_provenance_digest,
        effective_capabilities: production_owner_capabilities()?,
        grant_expires_at: UtcMicros(
            now_micros()
                .0
                .saturating_add(i64::try_from(GRANT_HORIZON.as_micros()).unwrap_or(i64::MAX)),
        ),
    })
}

fn production_owner_capabilities()
-> std::result::Result<BTreeSet<CapabilityId>, ApplicationContractError> {
    let mut capabilities = BTreeSet::new();
    for capability in [
        FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
        FEEDBACK_GET_CAPABILITY_ID_V1,
        FEEDBACK_EXPAND_CAPABILITY_ID_V1,
        FEEDBACK_LIST_CAPABILITY_ID_V1,
        "capability.application.feedback.impact",
        "capability.application.feedback.affected-tests",
        "capability.application.feedback.test-results",
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        PROXIMITY_CAPABILITY_ID_V1,
        "capability.application.primitive.session-lookup",
        "capability.application.primitive.qualified-name",
        "capability.application.primitive.call-chain",
        "capability.application.primitive.file-dependents",
        "capability.application.primitive.source-lines",
        "capability.application.primitive.source-body",
        "capability.application.primitive.source-outline",
        "capability.application.primitive.module-api",
        "capability.application.primitive.file-metadata",
        "capability.application.primitive.health-read",
        "capability.application.primitive.storage-status",
        "capability.application.primitive.diagnostics-read",
    ] {
        capabilities.insert(CapabilityId::new(capability.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open capability",
            }
        })?);
    }
    Ok(capabilities)
}

fn resolved_scope_for_project(
    project_root: &Path,
    project_id: &ProjectId,
) -> std::result::Result<ResolvedScope, ApplicationContractError> {
    let repository_digest = locator_digest_for_project(project_root)?;
    let repository_id = RepositoryId::new(format!(
        "repository.{}",
        repository_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open repository id",
    })?;
    let worktree_id = worktree_id_for_project(project_root)?;
    let reference = crate::branch::current_branch(project_root)
        .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
    ResolvedScope::new(project_id.clone(), repository_id, worktree_id, reference).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open resolved scope",
        }
    })
}

fn now_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
    )
}
