//! Structural rewrite through the bundled in-process ast-grep engine, applied
//! through the same commit-or-preview gate every other primitive uses.

use std::path::Path;

use crate::SourceEditFileAuthority;
use tracedecay_code_index::ast_grep_search::rewrite_source;
use tracedecay_contracts::source_edit::AstGrepResult;
use tracedecay_domain::errors::Result;

use super::preview::edit_success_message;

#[tracing::instrument(name = "edits.ast_grep_rewrite", level = "trace", skip_all)]
pub(crate) async fn ast_grep_rewrite(
    project_root: &Path,
    path: &str,
    pattern: &str,
    rewrite: &str,
    dry_run: bool,
) -> Result<AstGrepResult> {
    let rel_path = super::primitives::require_project_path(project_root, path)?;
    let file = SourceEditFileAuthority::open(project_root, Path::new(&rel_path))?;
    let (source, source_identity) = file.read_to_string(path)?;
    let refused = |message: String| AstGrepResult {
        success: false,
        file_path: rel_path.clone(),
        pattern: pattern.to_string(),
        rewrite: rewrite.to_string(),
        dry_run,
        diff: None,
        message,
    };
    let modified = match rewrite_source(Path::new(&rel_path), &source, pattern, rewrite) {
        Ok(Some(modified)) => modified,
        Ok(None) => {
            return Ok(refused(format!(
                "pattern {pattern:?} matched no syntax nodes in {rel_path}"
            )));
        }
        Err(message) => return Ok(refused(message)),
    };
    let diff = super::primitives::commit_or_preview_edit(
        &rel_path,
        &file,
        &source_identity,
        &source,
        &modified,
        dry_run,
    )
    .await?;
    Ok(AstGrepResult {
        success: true,
        file_path: rel_path,
        pattern: pattern.to_string(),
        rewrite: rewrite.to_string(),
        dry_run,
        diff,
        message: edit_success_message(dry_run, "ast-grep rewrite completed"),
    })
}
