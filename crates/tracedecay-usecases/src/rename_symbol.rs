//! Safe symbol-rename handoff into the canonical source-edit transaction.
//!
//! LSP contributes read-only candidate evidence only. This module resolves
//! that candidate back through the canonical graph, creates the same immutable
//! Plan 34 manifest used by API migration, and requires the caller to echo the
//! exact digest before the existing journaled source-edit authority can apply
//! it.

use std::path::Path;

use tracedecay_application::{
    ApiMigrationOperationRequestV1, ApiMigrationPlanRequestV1, ApiMigrationPlanV1,
    ApiMigrationSymbolV1, SourceEditRequest,
};
use tracedecay_domain::ManifestDigest;
use tracedecay_lsp::{RenameCandidate, utf16_position_to_byte_offset};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

use crate::tracedecay::TraceDecay;

/// Resolve an analyzer/graph-agreed LSP candidate into an immutable Plan 34
/// rename. The candidate can never apply an edit by itself.
pub async fn plan_lsp_rename_candidate(
    graph: &TraceDecay,
    candidate: &RenameCandidate,
    new_name: &str,
) -> Result<ApiMigrationPlanV1> {
    let uri = url::Url::parse(&candidate.document_uri)
        .map_err(|_| config_error("rename candidate document URI is invalid"))?;
    let absolute = uri
        .to_file_path()
        .map_err(|()| config_error("rename candidate document URI is not a file"))?;
    let relative = absolute
        .strip_prefix(graph.project_root())
        .map_err(|_| config_error("rename candidate is outside the admitted worktree"))?;
    let relative = relative_path(relative)?;
    let bytes =
        crate::tracedecay::read_source_edit_candidate(graph.project_root(), Path::new(&relative))?
            .ok_or_else(|| config_error("rename candidate source is missing"))?;
    let source = String::from_utf8(bytes)
        .map_err(|_| config_error("rename candidate source is not UTF-8"))?;
    let start = utf16_position_to_byte_offset(&source, candidate.range.start)
        .map_err(|_| config_error("rename candidate start is stale"))?;
    let end = utf16_position_to_byte_offset(&source, candidate.range.end)
        .map_err(|_| config_error("rename candidate end is stale"))?;
    if source.get(start..end) != Some(candidate.placeholder.as_str()) {
        return Err(config_error(
            "rename candidate placeholder no longer matches clean source",
        ));
    }
    let candidate_line = candidate.range.start.line.saturating_add(1);
    let mut nodes = graph
        .get_nodes_by_name(&candidate.placeholder)
        .await?
        .into_iter()
        .filter(|node| {
            node.file_path == relative
                && node.start_line <= candidate_line
                && node.end_line >= candidate_line
        });
    let node = nodes
        .next()
        .ok_or_else(|| config_error("rename candidate has no canonical graph symbol"))?;
    if nodes.next().is_some() {
        return Err(config_error(
            "rename candidate canonical graph identity is ambiguous",
        ));
    }
    let symbol = ApiMigrationSymbolV1 {
        node_id: node.id,
        qualified_name: node.qualified_name,
        kind: node.kind.as_str().to_owned(),
        file: node.file_path,
        old_name: node.name,
    };
    crate::api_migration::plan_api_migration(
        graph,
        ApiMigrationPlanRequestV1 {
            family_id: format!("rename-symbol.{}", symbol.node_id),
            operations: vec![ApiMigrationOperationRequestV1::RenameBoundSymbol {
                operation_id: "rename-symbol".to_owned(),
                depends_on: Vec::new(),
                symbol,
                new_name: new_name.to_owned(),
            }],
        },
    )
    .await
}

/// Build the only request admitted by `tracedecay_rename_symbol`.
///
/// `plan_digest` is explicit acceptance of the immutable preview. A family
/// plan or a mismatched digest is rejected before daemon admission.
pub fn tracedecay_rename_symbol(
    plan: ApiMigrationPlanV1,
    plan_digest: ManifestDigest,
    dry_run: bool,
) -> Result<SourceEditRequest> {
    if !matches!(
        plan.operations.as_slice(),
        [ApiMigrationOperationRequestV1::RenameBoundSymbol { .. }]
    ) || plan.blocked
        || plan.plan_digest != plan_digest
    {
        return Err(config_error(
            "rename symbol requires one accepted, unblocked bound-symbol plan",
        ));
    }
    Ok(SourceEditRequest::RenameSymbol {
        plan,
        plan_digest,
        dry_run,
        verify: true,
    })
}

fn relative_path(path: &Path) -> Result<String> {
    let rendered = path
        .to_str()
        .ok_or_else(|| config_error("rename candidate path is not UTF-8"))?
        .replace('\\', "/");
    if rendered.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(config_error("rename candidate path is unsafe"));
    }
    Ok(rendered)
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_application::{
        ApiMigrationFilePlanV1, ApiMigrationPlanV1, api_migration_file_digest,
    };

    use super::*;

    #[test]
    fn rename_apply_refuses_a_family_migration() {
        let mut plan = ApiMigrationPlanV1 {
            family_id: "family".to_owned(),
            repository_revision: "revision".to_owned(),
            graph_revision: api_migration_file_digest("graph").unwrap(),
            operations: Vec::new(),
            sites: Vec::new(),
            files: vec![ApiMigrationFilePlanV1 {
                path: "src/lib.rs".to_owned(),
                expected_digest: api_migration_file_digest("").unwrap(),
                predicted_digest: api_migration_file_digest("").unwrap(),
                expected_content: String::new(),
                intended_content: String::new(),
            }],
            blocked: false,
            plan_digest: api_migration_file_digest("pending").unwrap(),
        };
        plan.plan_digest = plan.compute_digest().unwrap();

        assert!(tracedecay_rename_symbol(plan.clone(), plan.plan_digest.clone(), false).is_err());
    }
}
