//! Name table for the structural-analysis report family.

use std::path::Path;

use serde_json::Value;
use tracedecay_domain::errors::Result;

#[cfg(feature = "source-analysis")]
use super::handle_unmounted_files;
use super::{
    handle_circular, handle_complexity, handle_constructors, handle_coupling, handle_dead_code,
    handle_distribution, handle_doc_coverage, handle_field_sites, handle_god_class,
    handle_hotspots, handle_inheritance_depth, handle_largest, handle_rank, handle_recursion,
    handle_unsafe_patterns,
};
use crate::ToolResult;
use crate::handlers::support::unknown_tool_error;
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};

/// Dispatches one analysis tool (`tracedecay_dead_code`,
/// `tracedecay_complexity`, ...) onto its handler over the `health_read`
/// verified graph opened through `open`.
pub async fn dispatch_tool(
    project_root: &Path,
    open: &VerifiedGraphOpen<'_>,
    tool_name: &str,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_dead_code" => {
            handle_dead_code(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_circular" => handle_circular(&open(read("health_read")?).await?, args).await,
        "tracedecay_hotspots" => {
            handle_hotspots(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        // The one analysis tool that opens no graph query: its whole finding is
        // that the graph and the compiler disagree, so taking the graph's file
        // set as input would answer the question with the very source that is
        // under suspicion.
        #[cfg(feature = "source-analysis")]
        "tracedecay_unmounted_files" => {
            handle_unmounted_files(project_root, args, scope_prefix).await
        }
        "tracedecay_rank" => {
            handle_rank(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_largest" => {
            handle_largest(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_coupling" => {
            handle_coupling(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_inheritance_depth" => {
            handle_inheritance_depth(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_distribution" => {
            handle_distribution(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_recursion" => {
            handle_recursion(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_complexity" => {
            handle_complexity(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_doc_coverage" => {
            handle_doc_coverage(
                project_root,
                &open(read("health_read")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_god_class" => {
            handle_god_class(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_unsafe_patterns" => {
            handle_unsafe_patterns(
                project_root,
                &open(read("health_read")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_constructors" => {
            handle_constructors(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        "tracedecay_field_sites" => {
            handle_field_sites(&open(read("health_read")?).await?, args, scope_prefix).await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}
