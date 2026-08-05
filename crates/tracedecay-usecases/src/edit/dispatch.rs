use tracedecay_application::SourceEditRequest;

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::Result;

use super::control::SourceEditEffectControlV1;
use super::outcome::SourceEditOutcome;

pub(super) async fn run_source_edit(
    graph: &TraceDecay,
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
    })
}
