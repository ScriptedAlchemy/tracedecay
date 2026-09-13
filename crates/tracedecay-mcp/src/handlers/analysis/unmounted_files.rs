//! `tracedecay_unmounted_files` — source files on disk that nothing declares.
//!
//! The reachability audit itself is
//! [`tracedecay_code_index::unmounted_files`]; this handler only reads the
//! tool arguments, filters and pages the findings, and renders them.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_code_index::unmounted_files::{EcosystemAudit, audit_project};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::ToolResult;
use crate::handlers::support::{
    effective_path, rendered_tool_result, require_object_args, unique_file_paths,
};
use crate::tools::render;

/// Default and ceiling for reported orphans in one response.
///
/// Unlike the paged import scan there is no cursor: the whole reachability
/// graph must be walked to know that *any* file is unmounted, so a second page
/// would repeat the entire walk for a suffix of the same answer. The response
/// states the true total and how many rows it omitted instead of pretending the
/// returned list is the whole finding.
const UNMOUNTED_FILES_DEFAULT_LIMIT: usize = 200;
const UNMOUNTED_FILES_MAX_LIMIT: usize = 2_000;

#[hotpath::measure(future = true, label = "mcp.analysis.unmounted_files.total")]
pub async fn handle_unmounted_files(
    project_root: &Path,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_unmounted_files")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(UNMOUNTED_FILES_DEFAULT_LIMIT, |limit| {
            (limit as usize).clamp(1, UNMOUNTED_FILES_MAX_LIMIT)
        });
    let path_filter = effective_path(&args, scope_prefix).map(str::to_owned);
    let ecosystem_filter = args
        .get("ecosystem")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);

    let scan_project_root = project_root.to_path_buf();
    // The walk reads every candidate source file, so it runs on a blocking
    // worker rather than holding the async dispatch thread through thousands
    // of synchronous reads.
    let audit = hotpath::future!(
        tokio::task::spawn_blocking(move || audit_project(&scan_project_root)),
        label = "mcp.analysis.unmounted_files.scan"
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("unmounted-file audit did not complete: {error}"),
    })??;

    let (output, touched_files) =
        hotpath::measure_block!("mcp.analysis.unmounted_files.assemble", {
            let matching = audit
                .ecosystems
                .iter()
                .filter(|ecosystem| {
                    ecosystem_filter
                        .as_deref()
                        .is_none_or(|wanted| wanted == ecosystem.ecosystem)
                })
                .flat_map(|ecosystem| {
                    ecosystem
                        .unmounted
                        .iter()
                        .map(move |entry| (ecosystem.ecosystem, entry))
                })
                .filter(|(_, entry)| {
                    tracedecay_runtime_core::path_scope::path_matches_scope(
                        &entry.file,
                        path_filter.as_deref(),
                    )
                })
                .collect::<Vec<_>>();
            let unmounted_file_count = matching.len();
            let returned = matching.iter().take(limit).collect::<Vec<_>>();
            let touched_files =
                unique_file_paths(returned.iter().map(|(_, entry)| entry.file.as_str()));

            let rows = returned
                .iter()
                .map(|(ecosystem, entry)| {
                    json!({
                        "file": entry.file,
                        "ecosystem": ecosystem,
                        "package": entry.package,
                        "manifest": entry.manifest,
                        "nearest_mounted_parent": entry.nearest_mounted_parent,
                        "suggested_declaration": entry.suggested_declaration,
                    })
                })
                .collect::<Vec<_>>();

            (
                json!({
                    "unmounted_file_count": unmounted_file_count,
                    "returned_count": rows.len(),
                    "omitted_count": unmounted_file_count.saturating_sub(rows.len()),
                    "complete": rows.len() == unmounted_file_count,
                    "ecosystems": audit
                        .ecosystems
                        .iter()
                        .map(ecosystem_json)
                        .collect::<Vec<_>>(),
                    "limit": limit,
                    "path": path_filter,
                    "ecosystem": ecosystem_filter,
                    "unmounted": rows,
                }),
                touched_files,
            )
        });

    Ok(rendered_tool_result(
        Some(project_root),
        &args,
        &output,
        touched_files,
        || render::unmounted_files_md(&output),
    ))
}

fn ecosystem_json(audit: &EcosystemAudit) -> Value {
    json!({
        "ecosystem": audit.ecosystem,
        "status": audit.status.as_str(),
        "package_count": audit.package_count,
        "entry_point_count": audit.entry_point_count,
        "scanned_file_count": audit.scanned_file_count,
        "mounted_file_count": audit.mounted_file_count,
        "unclaimed_file_count": audit.unclaimed_file_count,
        "unmounted_file_count": audit.unmounted.len(),
        "verdict": audit.verdict,
        "blind_spots": audit.blind_spots,
        "note": audit.note,
        "excluded_path_globs": audit.excluded_globs,
    })
}
