//! Project-open owner that runs the project's own TypeScript compiler when
//! each code-index generation seals and publishes its findings for it.
//!
//! `tracedecay_diagnostics` is a read over generation-bound publications, so a
//! freshly initialized TypeScript project answered "no producer" until a caller
//! pasted compiler output into `tracedecay_diagnose`. This owner is the
//! automatic producer for that case: it discovers the project's TypeScript
//! projects (each package's tsconfig in a monorepo) as bounded background
//! work after admission, runs only while at least one of them has its own
//! `node_modules/.bin/tsc`, and never installs anything. When none does, it
//! publishes that discovery, so reads name the missing compiler or tsconfig
//! from the producer's record rather than from a reading of the checkout.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_application::diagnostics_producer::{
    CompilerProducerRunV1, TypeScriptProducerStateV1, TypeScriptProducerStatusHandleV1,
    publish_typescript_discovery_v1, run_typescript_producer_v1,
};
use tracedecay_application::diagnostics_store::DiagnosticsStore;
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexOwnerSignalsV1;
use tracedecay_contracts::now_micros;
use tracedecay_domain::CodeGenerationId;
use tracedecay_lsp::{TypeScriptProject, TypeScriptProjectDiscoveryError, typescript_projects};
use tracedecay_runtime_core::logging::log_daemon_event;

use crate::daemon::invocation_state::DaemonInvocationState;

/// The project store's directory for tsc's incremental build-info, one file
/// per checked tsconfig; it leaves with the store.
const BUILD_INFO_DIR: &str = "typescript-build-info";

/// Spawns the TypeScript producer for `project_root`. Returns `false` when
/// nothing was spawned: a route that indexes no code by contract, so there is
/// never a generation to publish against. The spawned task publishes its
/// discovery and ends at once when no TypeScript project under the root has a
/// compiler; reads then describe the missing compiler from that record.
pub(super) fn spawn_typescript_diagnostics_producer(
    owner: &crate::mcp::McpServer,
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    scope: &tracedecay_contracts::ResolvedScope,
    graph: Arc<tracedecay_project::project::TraceDecay>,
    status: TypeScriptProducerStatusHandleV1,
) -> bool {
    if tracedecay_code_index_runtime::project_reads::code_index_disabled_for_scope(
        &invocation.code_index_schedulers,
        scope,
    ) {
        log_producer_event(&project_root, "code_index_disabled");
        return false;
    }
    let Some(admission) = invocation.compiler_admission.get().cloned() else {
        log_producer_event(&project_root, "compiler_admission_unavailable");
        return false;
    };
    // Reopen can retain the primitive runtime while the prior server's
    // compiler task is still retiring. Fence it before this task can yield
    // in discovery; scheduling order must not choose the current producer.
    let mut state = TypeScriptProducerStateV1::new(status, admission);
    owner.spawn_background_task(tracing::Instrument::instrument(
        async move {
            let schedulers = invocation.code_index_schedulers.clone();
            let mut signals = CodeIndexOwnerSignalsV1::subscribe(&schedulers, &project_root).await;
            // Subscribe before discovery so a seal during a failed walk still
            // wakes this owner after the tree becomes readable again.
            if let Some(projects) = discover_projects(&project_root).await {
                let compilers = projects
                    .iter()
                    .filter(|project| project.compiler.is_some())
                    .count();
                let uncheckable =
                    publish_typescript_discovery_v1(&project_root, &projects, &mut state).await;
                if compilers == 0 {
                    log_producer_event(
                        &project_root,
                        if projects.is_empty() {
                            "no_tsconfig"
                        } else {
                            "no_compiler"
                        },
                    );
                    if let Some(run) = uncheckable {
                        log_producer_run(&project_root, &run);
                    }
                    return;
                }
                log_daemon_event(
                    "typescript_diagnostics_producer",
                    &[
                        ("project", project_root.display().to_string()),
                        ("outcome", "admitted".to_owned()),
                        ("projects", projects.len().to_string()),
                        ("compilers", compilers.to_string()),
                    ],
                );
            }
            let build_info_dir = graph.store_layout().data_root.join(BUILD_INFO_DIR);
            // One producer run per generation, whatever its outcome: the root's
            // signals also wake on serving and pass changes, and a project
            // whose compiler keeps failing must not re-run it on each of them.
            let mut attempted_for: Option<CodeGenerationId> = None;
            loop {
                // The sealed generation, not the serving one: tsc reads only
                // the source the seal proved, so the check starts at the seal
                // rather than after the text projection and serving swap. A
                // generation sealed mid-run is caught by the next wake.
                match schedulers
                    .sealed_publication_identity(&project_root, attempted_for.as_ref())
                    .await
                {
                    Ok(Some(identity)) => {
                        attempted_for = Some(identity.generation_id().clone());
                        // A generation can add, drop, or retarget tsconfigs.
                        if let Some(projects) = discover_projects(&project_root).await {
                            let database = graph.dashboard_database_guard();
                            let store = DiagnosticsStore::new(database.as_ref().clone());
                            let run = run_typescript_producer_v1(
                                &project_root,
                                &projects,
                                &identity,
                                &store,
                                &build_info_dir,
                                &mut state,
                                now_micros(),
                            )
                            .await;
                            log_producer_run(&project_root, &run);
                        }
                    }
                    Ok(_) => {}
                    Err(error) => log_daemon_event(
                        "typescript_diagnostics_producer",
                        &[
                            ("project", project_root.display().to_string()),
                            ("outcome", "sealed_identity_unavailable".to_owned()),
                            ("detail", error.to_string()),
                        ],
                    ),
                }
                if signals.changed().await.is_err() {
                    return;
                }
            }
        },
        tracing::trace_span!("daemon.project.owners.typescript_diagnostics_producer"),
    ))
}

/// Walks the tree off the async runtime; `None` (logged) when the walk could
/// not complete. The owner retains its subscription and retries on a later
/// sealed generation instead of publishing a partial project list.
async fn discover_projects(project_root: &Path) -> Option<Vec<TypeScriptProject>> {
    let root = project_root.to_path_buf();
    match tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        typescript_projects(&root)
    })
    .await
    {
        Ok(walked) => discovery_outcome(project_root, walked),
        Err(error) => {
            log_discovery_failed(project_root, &error);
            None
        }
    }
}

/// The projects a completed walk admits. A walk that failed its tree is not a
/// smaller discovery: `None` is logged, and discovery is what must be retried.
fn discovery_outcome(
    project_root: &Path,
    walked: Result<Vec<TypeScriptProject>, TypeScriptProjectDiscoveryError>,
) -> Option<Vec<TypeScriptProject>> {
    match walked {
        Ok(projects) => Some(projects),
        Err(error) => {
            log_discovery_failed(project_root, &error.to_string());
            None
        }
    }
}

fn log_discovery_failed(project_root: &Path, detail: &str) {
    log_daemon_event(
        "typescript_diagnostics_producer",
        &[
            ("project", project_root.display().to_string()),
            ("outcome", "discovery_failed".to_owned()),
            ("detail", detail.to_owned()),
        ],
    );
}

fn log_producer_event(project_root: &Path, outcome: &str) {
    log_daemon_event(
        "typescript_diagnostics_producer",
        &[
            ("project", project_root.display().to_string()),
            ("outcome", outcome.to_owned()),
        ],
    );
}

/// Every run outcome lands on the daemon event stream: this owner is detached
/// background work, and a compiler that keeps failing must be diagnosable
/// from the log rather than inferred from a read that stays pending.
fn log_producer_run(project_root: &Path, run: &CompilerProducerRunV1) {
    let (outcome, detail) = match run {
        CompilerProducerRunV1::Published {
            generation,
            inserted,
            unresolved,
            checked,
            reused,
        } => (
            "published",
            format!(
                "generation={} inserted={inserted} unresolved={unresolved} checked={checked} \
                 reused={reused}",
                generation.as_str()
            ),
        ),
        CompilerProducerRunV1::NoResolvableDiagnostics { unresolved } => {
            ("no_resolvable_diagnostics", unresolved.join("; "))
        }
        CompilerProducerRunV1::CodeIndexGenerationUnavailable => {
            ("code_index_generation_unavailable", String::new())
        }
        CompilerProducerRunV1::CompilerFailed { reason } => ("compiler_failed", reason.clone()),
        CompilerProducerRunV1::PublicationFailed { reason } => {
            ("publication_failed", reason.clone())
        }
        CompilerProducerRunV1::Superseded => ("superseded", String::new()),
    };
    log_daemon_event(
        "typescript_diagnostics_producer",
        &[
            ("project", project_root.display().to_string()),
            ("outcome", outcome.to_owned()),
            ("detail", detail),
        ],
    );
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use tracedecay_lsp::typescript_projects;

    use super::discovery_outcome;

    /// A subtree the walk cannot read is a failed discovery, never a smaller
    /// project list that reads as `no_tsconfig`/`no_compiler`, and discovery
    /// recovers once the tree is readable again.
    #[test]
    fn an_unreadable_subdirectory_fails_discovery_and_the_next_walk_recovers() {
        let temporary = tempfile::tempdir().expect("fixture root");
        let root = temporary.path();
        fs::create_dir_all(root.join("packages/app")).expect("readable package");
        fs::write(root.join("packages/app/tsconfig.json"), "{}").expect("readable tsconfig");
        let blocked = root.join("packages/blocked");
        fs::create_dir_all(blocked.join("nested")).expect("blocked package");
        fs::write(blocked.join("nested/tsconfig.json"), "{}").expect("blocked tsconfig");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000))
            .expect("drop traversal permission");

        let walked = typescript_projects(root);
        // The probe runs under the same permissions as the walk; a privileged
        // runner bypasses them, so the unreadable case cannot be staged there.
        if fs::read_dir(&blocked).is_err() {
            let error = walked.expect_err("an unreadable subtree fails the walk");
            assert_eq!(error.root(), root);
            assert!(
                error.to_string().contains(&root.display().to_string()),
                "a failed walk names its root: {error}"
            );
            assert!(
                discovery_outcome(root, Err(error)).is_none(),
                "a failed walk is never published as a discovery"
            );
        }

        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700))
            .expect("restore traversal permission");
        let recovered =
            discovery_outcome(root, typescript_projects(root)).expect("a readable walk admits");
        assert_eq!(
            recovered
                .iter()
                .map(|project| project.tsconfig.strip_prefix(root).expect("under the root"))
                .collect::<Vec<_>>(),
            [
                "packages/app/tsconfig.json",
                "packages/blocked/nested/tsconfig.json"
            ]
            .map(Path::new)
            .to_vec(),
            "discovery recovers every project, including the one it could not read"
        );
    }
}
