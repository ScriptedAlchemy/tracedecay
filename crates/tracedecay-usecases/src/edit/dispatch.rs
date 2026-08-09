use std::sync::Arc;

use tracedecay_application::{RequestContext, SourceEditRequest};
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::GraphCancellation;

use crate::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadRequest, map_code_graph_read_runtime_error,
};
use crate::tracedecay::{SourceEditGraphReadV1, TraceDecay};
use tracedecay_runtime_core::errors::Result;

use super::control::SourceEditEffectControlV1;
use super::outcome::SourceEditOutcome;

pub(super) struct SourceEditGraphReadAuthorityV1<'a> {
    pub(super) port: &'a dyn CodeGraphProjectionReadPort,
    pub(super) context: &'a RequestContext,
    pub(super) observed_at: UtcMicros,
    pub(super) cancellation: Arc<dyn GraphCancellation>,
}

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

pub(super) async fn run_source_edit(
    graph: &TraceDecay,
    graph_read: SourceEditGraphReadAuthorityV1<'_>,
    request: SourceEditRequest,
    _control: Option<&SourceEditEffectControlV1>,
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
        } => SourceEditOutcome::Edit(graph.replace_symbol(&symbol, &new_source, dry_run).await?),
        SourceEditRequest::InsertAtSymbol {
            symbol,
            content,
            position,
            dry_run,
            ..
        } => SourceEditOutcome::Insert(
            graph
                .insert_at_symbol(&symbol, &content, &position, dry_run)
                .await?,
        ),
        SourceEditRequest::MoveSymbol {
            symbol,
            dest_file,
            dry_run,
            update_references,
        } => SourceEditOutcome::Move(
            graph
                .move_symbol(&symbol, &dest_file, dry_run, update_references)
                .await?,
        ),
        SourceEditRequest::RenameSymbol {
            binding,
            new_name,
            dry_run,
            ..
        } => SourceEditOutcome::Rename(
            graph
                .rename_symbol(
                    admitted_graph(&graph_read).await?,
                    &binding,
                    &new_name,
                    dry_run,
                )
                .await?,
        ),
    })
}
