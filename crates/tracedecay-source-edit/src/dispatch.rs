use std::sync::Arc;

use tracedecay_contracts::{RequestContext, SourceEditRequest};
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::GraphCancellation;

use tracedecay_domain::errors::Result;
use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadRequest,
    map_code_graph_read_runtime_error,
};

use super::outcome::SourceEditOutcome;
use super::port::{SourceEditGraphReadV1, SourceEditRuntime};

#[hotpath::measure(label = "usecases.edit.graph_read", future = true)]
async fn admitted_graph(
    port: &dyn CodeGraphProjectionReadPort,
    context: &RequestContext,
    observed_at: UtcMicros,
    cancellation: &Arc<dyn GraphCancellation>,
) -> Result<SourceEditGraphReadV1> {
    let verified = port
        .open(CodeGraphReadRequest::new(
            context,
            observed_at,
            Arc::clone(cancellation),
        ))
        .await
        .map_err(map_code_graph_read_runtime_error)?;
    // Edits demand current evidence. The projection port's serve-stale arm
    // exists for reads: during a rebuild it answers from the last complete
    // seated generation. A symbol edit planned against that generation passes
    // the digest gate on unchanged files while silently missing call sites in
    // newly committed ones, so a stale-served open is a typed refusal here,
    // never an incomplete plan.
    if verified.freshness().is_stale() {
        return Err(map_code_graph_read_runtime_error(
            CodeGraphReadError::Stale {
                detail: format!(
                    "symbol edits require current graph evidence, but the code index is \
                     rebuilding and only the last complete generation {} is seated; retry \
                     after the rebuild completes",
                    verified.generation()
                ),
            },
        ));
    }
    let reader = verified
        .reader_with_cancellation(context, observed_at, Arc::clone(cancellation))
        .map_err(map_code_graph_read_runtime_error)?;
    Ok(SourceEditGraphReadV1::new(reader, Arc::clone(cancellation)))
}

#[hotpath::measure(label = "usecases.edit.dispatch", future = true)]
pub(super) async fn run_source_edit(
    graph: &SourceEditRuntime,
    port: &dyn CodeGraphProjectionReadPort,
    context: &RequestContext,
    observed_at: UtcMicros,
    cancellation: Arc<dyn GraphCancellation>,
    request: SourceEditRequest,
) -> Result<SourceEditOutcome> {
    Ok(match request {
        SourceEditRequest::StrReplace {
            path,
            old_str,
            new_str,
            dry_run,
            ..
        } => SourceEditOutcome::Edit(
            crate::edits::str_replace(graph.project_root(), &path, &old_str, &new_str, dry_run)
                .await?,
        ),
        SourceEditRequest::MultiStrReplace {
            path,
            replacements,
            dry_run,
            ..
        } => {
            let replacements = replacements
                .iter()
                .map(|(old, new)| (old.as_str(), new.as_str()))
                .collect::<Vec<_>>();
            SourceEditOutcome::MultiEdit(
                crate::edits::multi_str_replace(
                    graph.project_root(),
                    &path,
                    &replacements,
                    dry_run,
                )
                .await?,
            )
        }
        SourceEditRequest::InsertAt {
            path,
            anchor,
            content,
            before,
            dry_run,
            ..
        } => SourceEditOutcome::Insert(
            crate::edits::insert_at(
                graph.project_root(),
                &path,
                &anchor,
                &content,
                before,
                dry_run,
            )
            .await?,
        ),
        SourceEditRequest::AstGrepRewrite {
            path,
            pattern,
            rewrite,
            dry_run,
            ..
        } => SourceEditOutcome::AstGrep(
            crate::edits::ast_grep_rewrite(
                graph.project_root(),
                &path,
                &pattern,
                &rewrite,
                dry_run,
            )
            .await?,
        ),
        SourceEditRequest::ReplaceSymbol {
            symbol,
            new_source,
            dry_run,
            ..
        } => SourceEditOutcome::Edit(
            crate::edits::replace_symbol(
                graph.project_root(),
                admitted_graph(port, context, observed_at, &cancellation).await?,
                &symbol,
                &new_source,
                dry_run,
            )
            .await?,
        ),
        SourceEditRequest::InsertAtSymbol {
            symbol,
            content,
            position,
            dry_run,
            ..
        } => SourceEditOutcome::Insert(
            crate::edits::insert_at_symbol(
                graph.project_root(),
                admitted_graph(port, context, observed_at, &cancellation).await?,
                &symbol,
                &content,
                &position,
                dry_run,
            )
            .await?,
        ),
        SourceEditRequest::MoveSymbol {
            symbol,
            dest_file,
            dry_run,
            update_references,
        } => SourceEditOutcome::Move(
            crate::move_symbol::move_symbol(
                graph.project_root(),
                admitted_graph(port, context, observed_at, &cancellation).await?,
                &symbol,
                &dest_file,
                dry_run,
                update_references,
            )
            .await?,
        ),
        SourceEditRequest::RenameSymbol {
            binding,
            new_name,
            dry_run,
            ..
        } => SourceEditOutcome::Rename(Box::new(
            crate::edits::rename_symbol(
                graph.project_root(),
                admitted_graph(port, context, observed_at, &cancellation).await?,
                &binding,
                &new_name,
                dry_run,
            )
            .await?,
        )),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracedecay_code_index::graph_projection::{
        CodeGraphProjectionStore, HermeticCodeGraphProjectionStore,
    };
    use tracedecay_contracts::CancellationSignal;
    use tracedecay_domain::CodeGenerationId;
    use tracedecay_domain::errors::{Result, TraceDecayError};
    use tracedecay_graph_db::{GraphCancellation, NeverCancelled};
    use tracedecay_graph_query::{
        CodeGraphProjectionReadPort, CodeGraphReadFreshnessV1, CodeGraphReadFuture,
        CodeGraphReadRequest, VerifiedCodeGraphRead,
    };

    use super::admitted_graph;
    use crate::port::SourceEditGraphReadV1;
    use crate::test_support::fixture_request;

    struct FixtureGraphPort {
        store: Arc<CodeGraphProjectionStore>,
        freshness: CodeGraphReadFreshnessV1,
    }

    impl CodeGraphProjectionReadPort for FixtureGraphPort {
        fn open<'a>(&'a self, request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
            let store = Arc::clone(&self.store);
            let freshness = self.freshness;
            let scope = request.context.scope().clone();
            Box::pin(async move { VerifiedCodeGraphRead::new(scope, store, freshness) })
        }
    }

    fn seated_generation_store() -> Arc<CodeGraphProjectionStore> {
        let cancellation =
            CancellationSignal::active("cancel.edit.freshness.fixture").expect("cancellation");
        let projection =
            HermeticCodeGraphProjectionStore::memory(&cancellation).expect("memory projection");
        let generation =
            CodeGenerationId::new("generation.edit.freshness.fixture.1").expect("generation id");
        projection
            .publish_with_cancellation(&generation, &[], &[], Arc::new(NeverCancelled))
            .expect("publish fixture generation");
        Arc::new(
            projection
                .verified_store(&generation)
                .expect("open fixture generation"),
        )
    }

    async fn open_admitted(freshness: CodeGraphReadFreshnessV1) -> Result<SourceEditGraphReadV1> {
        let request = fixture_request();
        let port = FixtureGraphPort {
            store: seated_generation_store(),
            freshness,
        };
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
        admitted_graph(&port, &request.context, request.observed_at, &cancellation).await
    }

    /// The projection port's serve-stale arm keeps reads answering during a
    /// rebuild, but a symbol edit planned against the pre-rebuild generation
    /// silently misses call sites in newly committed files. The edit path
    /// must refuse a stale-served open with a typed, retryable error.
    #[tokio::test]
    async fn a_stale_served_graph_open_is_refused_by_the_edit_path() {
        let Err(error) = open_admitted(CodeGraphReadFreshnessV1::LastCompleteStale {
            sealed_at: tracedecay_domain::UtcMicros(1),
            rebuild_in_flight: true,
        })
        .await
        else {
            panic!("stale-served evidence must not reach the edit planner");
        };
        match error {
            TraceDecayError::ProjectRoute {
                reason_code,
                retryable,
                detail,
            } => {
                assert_eq!(reason_code, "code-graph-stale");
                assert!(retryable, "a rebuild in flight resolves itself: {detail}");
            }
            other => panic!("expected the typed stale refusal, got {other:?}"),
        }
    }
}
