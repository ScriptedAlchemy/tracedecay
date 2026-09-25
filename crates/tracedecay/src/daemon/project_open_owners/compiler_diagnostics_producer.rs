//! Project-open owner that runs the project's own TypeScript compiler after
//! each complete code-index generation and publishes its findings.
//!
//! `tracedecay_diagnostics` is a read over generation-bound publications, so a
//! freshly initialized TypeScript project answered "no producer" until a caller
//! pasted compiler output into `tracedecay_diagnose`. This owner is the
//! automatic producer for that case: it is admitted only when the project has
//! a `tsconfig.json` and `node_modules/.bin/tsc`, runs as bounded background
//! work after admission, and never installs anything.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_application::diagnostics_producer::{
    CompilerProducerRunV1, run_typescript_producer_v1,
};
use tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use tracedecay_application::diagnostics_store::DiagnosticsStore;
use tracedecay_contracts::now_micros;
use tracedecay_domain::CodeGenerationId;
use tracedecay_lsp::{TypeScriptProducerState, typescript_producer_state};
use tracedecay_runtime_core::logging::log_daemon_event;

use super::DaemonInvocationState;
use super::advisory_runtime::wait_for_generation_change;

/// Spawns the TypeScript producer for `project_root` when the project carries
/// its own compiler. Returns `false` when nothing was spawned: no
/// `tsconfig.json`, no `node_modules/.bin/tsc`, or a route that indexes no
/// code by contract, so there is never a generation to publish against.
pub(super) fn spawn_typescript_diagnostics_producer(
    owner: &crate::mcp::McpServer,
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    scope: &tracedecay_contracts::ResolvedScope,
    graph: Arc<tracedecay_project::project::TraceDecay>,
) -> bool {
    let compiler = match typescript_producer_state(&project_root) {
        TypeScriptProducerState::Configured { compiler } => compiler,
        TypeScriptProducerState::CompilerMissing | TypeScriptProducerState::NoTsconfig => {
            return false;
        }
    };
    if tracedecay_code_index_runtime::project_reads::code_index_disabled_for_scope(
        &invocation.code_index_schedulers,
        scope,
    ) {
        log_producer_event(&project_root, "code_index_disabled");
        return false;
    }
    log_daemon_event(
        "typescript_diagnostics_producer",
        &[
            ("project", project_root.display().to_string()),
            ("outcome", "admitted".to_owned()),
            ("compiler", compiler.display().to_string()),
        ],
    );
    owner.spawn_background_task(hotpath::future!(
        async move {
            let schedulers = invocation.code_index_schedulers.clone();
            let mut publications = schedulers.subscribe_generation_publications();
            let mut serving_seats = schedulers.subscribe_serving_seats();
            let mut root_mounted = schedulers.subscribe_root_mounted();
            let mut serving_changes = None;
            // One compiler run per generation, whatever its outcome: the
            // wake sources below are registry-wide, and a project whose
            // compiler keeps failing must not re-run it on every other
            // project's publication.
            let mut attempted_for: Option<CodeGenerationId> = None;
            loop {
                if serving_changes.is_none() {
                    serving_changes = schedulers
                        .subscribe_serving_generation_changes(&project_root)
                        .await;
                    if serving_changes.is_some() {
                        let _ = schedulers.request_complete_generation(&project_root).await;
                    }
                }
                // Resolve the generation first so an unchanged generation costs
                // no compiler run; the producer publishes against exactly the
                // identity it resolved, and a generation sealed mid-run is
                // caught by the next wake.
                let current =
                    CodeIndexPublicationIdentityPortV1::resolve(&schedulers, project_root.clone())
                        .await
                        .map(|identity| identity.generation_id().clone());
                if let Some(generation) = current
                    && attempted_for.as_ref() != Some(&generation)
                {
                    attempted_for = Some(generation);
                    let database = graph.dashboard_database_guard();
                    let store = DiagnosticsStore::new(database.as_ref().clone());
                    let run = run_typescript_producer_v1(
                        &project_root,
                        &compiler,
                        &schedulers,
                        &store,
                        now_micros(),
                    )
                    .await;
                    log_producer_run(&project_root, &run);
                }
                if !wait_for_generation_change(
                    &project_root,
                    &mut publications,
                    &mut serving_changes,
                    &mut serving_seats,
                    &mut root_mounted,
                )
                .await
                {
                    return;
                }
            }
        },
        label = "daemon.project.owners.typescript_diagnostics_producer"
    ))
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
        } => (
            "published",
            format!(
                "generation={} inserted={inserted} unresolved={unresolved}",
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
