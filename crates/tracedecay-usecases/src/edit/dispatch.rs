use tracedecay_application::{
    ApiMigrationPlanRequestV1, ApiMigrationPlanV1, CancellationStage, SourceEditRequest,
};

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::Result;

use super::control::SourceEditEffectControlV1;
use super::outcome::SourceEditOutcome;
use super::verify::config_error;

pub(super) async fn run_source_edit(
    graph: &TraceDecay,
    request: SourceEditRequest,
    control: Option<&SourceEditEffectControlV1>,
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
        SourceEditRequest::ApiMigrationApply {
            plan,
            plan_digest,
            dry_run,
            ..
        } => {
            if plan.plan_digest != plan_digest {
                return Err(config_error(
                    "API migration apply digest does not match its immutable plan",
                ));
            }
            let replanned = crate::api_migration::plan_api_migration(
                graph,
                ApiMigrationPlanRequestV1 {
                    family_id: plan.family_id.clone(),
                    operations: plan.operations.clone(),
                },
            )
            .await?;
            validate_replanned_api_migration(&plan, &replanned)?;
            let mut is_cancelled = || {
                control
                    .and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
                    .is_some()
            };
            SourceEditOutcome::ApiMigration(
                graph
                    .apply_api_migration_plan(&replanned, dry_run, &mut is_cancelled)
                    .await?,
            )
        }
    })
}

fn validate_replanned_api_migration(
    supplied: &ApiMigrationPlanV1,
    replanned: &ApiMigrationPlanV1,
) -> Result<()> {
    if supplied != replanned {
        return Err(config_error(
            "API migration plan does not match current graph-backed evidence; replan before apply",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::test_support::*;

    #[test]
    fn api_migration_apply_requires_the_current_graph_backed_plan() {
        let supplied = ApiMigrationPlanV1 {
            family_id: "family".to_owned(),
            repository_revision: "revision".to_owned(),
            graph_revision: digest(SHA256_A),
            operations: Vec::new(),
            sites: Vec::new(),
            files: Vec::new(),
            blocked: false,
            plan_digest: digest(SHA256_B),
        };
        let mut replanned = supplied.clone();
        assert!(validate_replanned_api_migration(&supplied, &replanned).is_ok());
        replanned.graph_revision = digest(SHA256_B);
        assert!(validate_replanned_api_migration(&supplied, &replanned).is_err());
    }
}
