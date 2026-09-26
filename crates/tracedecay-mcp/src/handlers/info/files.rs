//! `tracedecay_files`, indexed file listing with prefix and glob filters.

use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    FilesLayoutV1, FilesResultV1, FilesSurfaceRequestV1, IndexedFileV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

use crate::handlers::graph::graph_tool_completion;
use crate::path_tree::format_compact_annotated_path_list;
use crate::tools::render::Md;

use super::verified::indexed_files;

#[hotpath::measure(future = true, label = "mcp.info.files.total")]
pub async fn compute_files(
    graph: &VerifiedGraphQuery,
    request: FilesSurfaceRequestV1,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let project_root = graph.project_root()?;
    let mut files = hotpath::future!(
        indexed_files(project_root, graph),
        label = "mcp.info.files.census"
    )
    .await?;

    if let Some(dir) = request.path.as_deref().or(scope_prefix) {
        files.retain(|f| tracedecay_domain::path_matches_scope(&f.path, Some(dir)));
    }

    if let Some(pat) = request.pattern.as_deref() {
        let glob = glob::Pattern::new(pat).map_err(|error| TraceDecayError::Config {
            message: format!("invalid file glob '{pat}': {error}"),
        })?;
        files.retain(|f| glob.matches(&f.path));
    }

    let files = files
        .into_iter()
        .map(|file| IndexedFileV1 {
            path: file.path,
            symbols: u64::from(file.node_count),
            bytes: file.size,
        })
        .collect::<Vec<_>>();
    // Listing files is metadata-only, no source code is served, so no tokens saved.
    Ok(graph_tool_completion(
        GraphToolResultV1::Files(FilesResultV1 {
            count: files.len(),
            layout: request.layout.unwrap_or_default(),
            files,
        }),
        Vec::new(),
    ))
}

pub(crate) fn render_files_md(result: &FilesResultV1) -> String {
    let layout = match result.layout {
        FilesLayoutV1::Flat => "flat",
        FilesLayoutV1::Grouped => "grouped",
    };
    let mut md = Md::new();
    md.heading(2, "Files");
    md.field("indexed files", &result.files.len().to_string());
    md.field("layout", layout);

    if result.files.is_empty() {
        md.blank().empty_note("No indexed files matched.");
        return md.render();
    }

    if result.layout == FilesLayoutV1::Flat {
        let listing = result
            .files
            .iter()
            .map(|file| {
                format!(
                    "- {} ({} symbols, {} bytes)",
                    file.path, file.symbols, file.bytes
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        md.blank().code("text", &listing);
        return md.render();
    }

    let suffixes = result
        .files
        .iter()
        .map(|file| format!(" ({} symbols)", file.symbols))
        .collect::<Vec<_>>();
    let annotated = result
        .files
        .iter()
        .zip(suffixes.iter())
        .map(|(file, suffix)| (file.path.as_str(), suffix.as_str()));
    let listing = format_compact_annotated_path_list(annotated, "- ", "");
    md.blank().code("text", &listing);
    md.render()
}
