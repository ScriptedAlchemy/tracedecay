//! `tracedecay_dsm` — design-structure matrix over file dependencies.

use super::*;

/// Handles `tracedecay_dsm` tool calls.
pub(crate) async fn handle_dsm(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let shape = args
        .get("shape")
        .and_then(|v| v.as_str())
        .unwrap_or("stats");
    let max_files = args
        .get("max_files")
        .and_then(serde_json::Value::as_u64)
        .map_or(30, |v| v.min(200) as usize);

    let adj = graph.build_file_adjacency(path_prefix).await?;

    let file_count = adj.len();
    let edge_count: usize = adj.values().map(std::collections::HashSet::len).sum();
    let density = if file_count > 1 {
        edge_count as f64 / (file_count * (file_count - 1)) as f64
    } else {
        0.0
    };

    let cluster_rows = dsm_clusters(&adj);
    let mut clusters: Vec<Value> = cluster_rows
        .iter()
        .map(|cluster| {
            json!({
                "directory": cluster.directory,
                "file_count": cluster.file_count,
                "internal_edges": cluster.internal_edges,
                "outgoing_edges": cluster.outgoing_edges,
                "incoming_edges": cluster.incoming_edges,
                "boundary_edges": cluster.boundary_edges(),
            })
        })
        .collect();
    let largest_cluster = clusters
        .iter()
        .filter_map(|cluster| cluster["file_count"].as_u64())
        .max()
        .unwrap_or(0);
    let stats = json!({
        "files": file_count,
        "edges": edge_count,
        "density": (density * 10000.0).round() / 10000.0,
        "clusters": cluster_rows.len(),
        "largest_cluster": largest_cluster,
    });
    let output = match shape {
        "clusters" => json!({
            "shape": "clusters",
            "stats": stats,
            "clusters": clusters,
        }),
        "matrix" => json!({
            "shape": "matrix",
            "stats": stats,
            "clusters": clusters.into_iter().take(10).collect::<Vec<_>>(),
            "matrix": dsm_matrix(&adj, max_files),
        }),
        _ => {
            clusters.truncate(10);
            json!({
                "shape": "stats",
                "stats": stats,
                "clusters": clusters,
            })
        }
    };

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render_dsm_md(&output),
    ))
}

fn dsm_matrix(adj: &HashMap<String, HashSet<String>>, max_files: usize) -> Value {
    let mut file_edge_counts: Vec<(String, usize)> = adj
        .iter()
        .map(|(f, targets)| {
            let out = targets.len();
            let inc = adj.values().filter(|t| t.contains(f)).count();
            (f.clone(), out + inc)
        })
        .collect();
    file_edge_counts.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    file_edge_counts.truncate(max_files);

    let selected: Vec<String> = file_edge_counts.into_iter().map(|(f, _)| f).collect();
    let short_names: Vec<String> = selected
        .iter()
        .map(|f| {
            f.rfind('/')
                .map_or_else(|| f.clone(), |i| f[i + 1..].to_string())
        })
        .collect();

    let n = selected.len();
    let mut matrix: Vec<Vec<u8>> = vec![vec![0u8; n]; n];
    for (i, src) in selected.iter().enumerate() {
        if let Some(targets) = adj.get(src) {
            for (j, tgt) in selected.iter().enumerate() {
                if i != j && targets.contains(tgt) {
                    matrix[i][j] = 1;
                }
            }
        }
    }

    json!({
        "files": short_names,
        "matrix": matrix,
        "note": format!("Top {} files by edge count shown", n),
    })
}

fn render_dsm_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Design Structure Matrix");
    md.field("shape", render::field_str(value, "shape"));
    if let Some(stats) = value.get("stats") {
        md.field("files", &render::field_i64(stats, "files").to_string());
        md.field("edges", &render::field_i64(stats, "edges").to_string());
        md.field("density", render::field_str(stats, "density"));
        md.field(
            "clusters",
            &render::field_i64(stats, "clusters").to_string(),
        );
        md.field(
            "largest_cluster",
            &render::field_i64(stats, "largest_cluster").to_string(),
        );
    }

    if let Some(clusters) = value.get("clusters").and_then(Value::as_array) {
        md.blank().heading(3, "Top Clusters");
        if clusters.is_empty() {
            md.empty_note("No dependency clusters found.");
        } else {
            for cluster in clusters.iter().take(10) {
                let dir = render::field_str(cluster, "directory");
                let files = render::field_i64(cluster, "file_count");
                let internal = render::field_i64(cluster, "internal_edges");
                let outgoing = render::field_i64(cluster, "outgoing_edges");
                let incoming = render::field_i64(cluster, "incoming_edges");
                let boundary = render::field_i64(cluster, "boundary_edges");
                md.bullet(&format!(
                    "{dir}: {files} files; {internal} internal; {boundary} boundary ({outgoing} out, {incoming} in)"
                ));
            }
        }
    }

    if let Some(matrix) = value.get("matrix") {
        md.blank().heading(3, "Matrix");
        if let Some(note) = matrix.get("note").and_then(Value::as_str) {
            md.field("note", note);
        }
        md.code(
            "json",
            &serde_json::to_string(matrix).unwrap_or_else(|_| "{}".to_string()),
        );
    }

    md.render()
}
