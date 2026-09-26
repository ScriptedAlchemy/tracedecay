//! `tracedecay_unmounted_files`, source files on disk that nothing declares.
//!
//! The reachability audit itself is
//! [`tracedecay_code_index::unmounted_files`]; this report only filters and
//! pages its findings into the typed result.

use std::path::Path;

use serde_json::Value;
use tracedecay_code_index::unmounted_files::{EcosystemAudit, EcosystemStatus, audit_project};
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    UnmountedEcosystemStatusV1, UnmountedEcosystemV1, UnmountedFileV1, UnmountedFilesResultV1,
    UnmountedFilesSurfaceRequestV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::handlers::graph::graph_tool_completion;
use crate::handlers::support::{decode_primitive_request, unique_file_paths};

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
pub(super) async fn compute_unmounted_files(
    project_root: &Path,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: UnmountedFilesSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_unmounted_files")?;
    let limit = request
        .limit
        .map_or(UNMOUNTED_FILES_DEFAULT_LIMIT, |limit| {
            (limit as usize).clamp(1, UNMOUNTED_FILES_MAX_LIMIT)
        });
    let path_filter = request.path.or_else(|| scope_prefix.map(str::to_owned));
    let ecosystem_filter = request.ecosystem.as_deref().map(str::to_ascii_lowercase);

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
            tracedecay_domain::path_matches_scope(&entry.file, path_filter.as_deref())
        })
        .collect::<Vec<_>>();
    let unmounted_file_count = matching.len();
    let unmounted: Vec<UnmountedFileV1> = matching
        .into_iter()
        .take(limit)
        .map(|(ecosystem, entry)| UnmountedFileV1 {
            file: entry.file.clone(),
            ecosystem: ecosystem.to_owned(),
            package: entry.package.clone(),
            manifest: entry.manifest.clone(),
            nearest_mounted_parent: entry.nearest_mounted_parent.clone(),
            suggested_declaration: entry.suggested_declaration.clone(),
        })
        .collect();
    let touched_files = unique_file_paths(unmounted.iter().map(|entry| entry.file.as_str()));
    let returned_count = unmounted.len();

    Ok(graph_tool_completion(
        GraphToolResultV1::UnmountedFiles(UnmountedFilesResultV1 {
            unmounted_file_count: unmounted_file_count as u64,
            returned_count: returned_count as u64,
            omitted_count: unmounted_file_count.saturating_sub(returned_count) as u64,
            complete: returned_count == unmounted_file_count,
            ecosystems: audit.ecosystems.iter().map(ecosystem_section).collect(),
            limit: limit as u64,
            path: path_filter,
            ecosystem: ecosystem_filter,
            unmounted,
        }),
        touched_files,
    ))
}

fn ecosystem_section(audit: &EcosystemAudit) -> UnmountedEcosystemV1 {
    UnmountedEcosystemV1 {
        ecosystem: audit.ecosystem.to_owned(),
        status: match audit.status {
            EcosystemStatus::Audited => UnmountedEcosystemStatusV1::Audited,
            EcosystemStatus::NotPresent => UnmountedEcosystemStatusV1::NotPresent,
            EcosystemStatus::Unsupported => UnmountedEcosystemStatusV1::Unsupported,
        },
        package_count: audit.package_count as u64,
        entry_point_count: audit.entry_point_count as u64,
        scanned_file_count: audit.scanned_file_count as u64,
        mounted_file_count: audit.mounted_file_count as u64,
        unclaimed_file_count: audit.unclaimed_file_count as u64,
        unmounted_file_count: audit.unmounted.len() as u64,
        verdict: audit.verdict.to_owned(),
        blind_spots: audit
            .blind_spots
            .iter()
            .map(|spot| (*spot).to_owned())
            .collect(),
        note: audit.note.clone(),
        excluded_path_globs: audit.excluded_globs.clone(),
    }
}
