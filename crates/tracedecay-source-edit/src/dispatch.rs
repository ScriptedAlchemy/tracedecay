use std::sync::Arc;

use tracedecay_application::{RequestContext, SourceEditRequest};
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::GraphCancellation;

use tracedecay_domain::errors::Result;
use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadRequest,
    map_code_graph_read_runtime_error,
};
use tracedecay_usecases::tracedecay::{SourceEditGraphReadV1, SourceEditRuntime};

use super::outcome::SourceEditOutcome;

pub(super) struct SourceEditGraphReadAuthorityV1<'a> {
    pub(super) port: &'a dyn CodeGraphProjectionReadPort,
    pub(super) context: &'a RequestContext,
    pub(super) observed_at: UtcMicros,
    pub(super) cancellation: Arc<dyn GraphCancellation>,
}

#[hotpath::measure(label = "usecases.edit.graph_read", future = true)]
async fn admitted_graph(
    authority: &SourceEditGraphReadAuthorityV1<'_>,
) -> Result<SourceEditGraphReadV1> {
    let verified = authority
        .port
        .open(CodeGraphReadRequest::new(
            authority.context,
            authority.observed_at,
            Arc::clone(&authority.cancellation),
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
        .reader_with_cancellation(
            authority.context,
            authority.observed_at,
            Arc::clone(&authority.cancellation),
        )
        .map_err(map_code_graph_read_runtime_error)?;
    Ok(SourceEditGraphReadV1::new(
        reader,
        Arc::clone(&authority.cancellation),
    ))
}

#[hotpath::measure(label = "usecases.edit.dispatch", future = true)]
pub(super) async fn run_source_edit(
    graph: &SourceEditRuntime,
    graph_read: SourceEditGraphReadAuthorityV1<'_>,
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
            graph
                .str_replace(&path, &old_str, &new_str, dry_run)
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
                graph
                    .multi_str_replace(&path, &replacements, dry_run)
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
            graph
                .insert_at(&path, &anchor, &content, before, dry_run)
                .await?,
        ),
        SourceEditRequest::AstGrepRewrite {
            path,
            pattern,
            rewrite,
            dry_run,
            ..
        } => SourceEditOutcome::AstGrep(
            graph
                .ast_grep_rewrite(&path, &pattern, &rewrite, dry_run)
                .await?,
        ),
        SourceEditRequest::ReplaceSymbol {
            symbol,
            new_source,
            dry_run,
            ..
        } => SourceEditOutcome::Edit(
            graph
                .replace_symbol(
                    admitted_graph(&graph_read).await?,
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
            graph
                .insert_at_symbol(
                    admitted_graph(&graph_read).await?,
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
            graph
                .move_symbol(
                    admitted_graph(&graph_read).await?,
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
            graph
                .rename_symbol(
                    admitted_graph(&graph_read).await?,
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

    use tracedecay_application::CancellationSignal;
    use tracedecay_code_index::graph_projection::{
        CodeGraphProjectionStore, HermeticCodeGraphProjectionStore,
    };
    use tracedecay_domain::CodeGenerationId;
    use tracedecay_domain::errors::{Result, TraceDecayError};
    use tracedecay_graph_db::NeverCancelled;
    use tracedecay_graph_query::{
        CodeGraphProjectionReadPort, CodeGraphReadFreshnessV1, CodeGraphReadFuture,
        CodeGraphReadRequest, VerifiedCodeGraphRead,
    };
    use tracedecay_usecases::tracedecay::SourceEditGraphReadV1;

    use super::{SourceEditGraphReadAuthorityV1, admitted_graph};
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
        let authority = SourceEditGraphReadAuthorityV1 {
            port: &port,
            context: &request.context,
            observed_at: request.observed_at,
            cancellation: Arc::new(NeverCancelled),
        };
        admitted_graph(&authority).await
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

    #[tokio::test]
    async fn a_current_graph_open_reaches_the_edit_planner() {
        open_admitted(CodeGraphReadFreshnessV1::Current)
            .await
            .expect("current evidence must admit the edit planner");
    }
}
