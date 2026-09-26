//! `tracedecay_dsm`, design-structure matrix over file dependencies.

use super::*;

#[hotpath::measure(label = "mcp.health.dsm.total")]
pub async fn compute_dsm(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: DsmSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_dsm")?;
    let path_prefix = request.path.as_deref().or(scope_prefix);
    let shape = request.shape.unwrap_or_default();
    let max_files = request.max_files.map_or(30, |v| v.min(200) as usize);

    let adj = hotpath::future!(
        graph.build_file_adjacency(path_prefix),
        label = "mcp.health.dsm.graph"
    )
    .await?;

    let (mut clusters, stats) = hotpath::measure_block!("mcp.health.dsm.compute", {
        let file_count = adj.len();
        let edge_count: usize = adj.values().map(std::collections::HashSet::len).sum();
        let density = if file_count > 1 {
            edge_count as f64 / (file_count * (file_count - 1)) as f64
        } else {
            0.0
        };

        let cluster_rows = dsm_clusters(&adj);
        let clusters: Vec<DsmClusterV1> = cluster_rows
            .iter()
            .map(|cluster| DsmClusterV1 {
                directory: cluster.directory.clone(),
                file_count: cluster.file_count as u64,
                internal_edges: cluster.internal_edges as u64,
                outgoing_edges: cluster.outgoing_edges as u64,
                incoming_edges: cluster.incoming_edges as u64,
                boundary_edges: cluster.boundary_edges() as u64,
            })
            .collect();
        let largest_cluster = cluster_rows
            .iter()
            .map(|cluster| cluster.file_count as u64)
            .max()
            .unwrap_or(0);
        let stats = DsmStatsV1 {
            files: file_count as u64,
            edges: edge_count as u64,
            density: (density * 10000.0).round() / 10000.0,
            clusters: cluster_rows.len() as u64,
            largest_cluster,
        };
        (clusters, stats)
    });
    let matrix = match shape {
        DsmShapeV1::Clusters => None,
        DsmShapeV1::Matrix => {
            clusters.truncate(10);
            Some(dsm_matrix(&adj, max_files))
        }
        DsmShapeV1::Stats => {
            clusters.truncate(10);
            None
        }
    };

    Ok(graph_tool_completion(
        GraphToolResultV1::Dsm(DsmResultV1 {
            shape,
            stats,
            clusters,
            matrix,
        }),
        Vec::new(),
    ))
}

fn dsm_matrix(adj: &HashMap<String, HashSet<String>>, max_files: usize) -> DsmMatrixV1 {
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

    DsmMatrixV1 {
        files: short_names,
        matrix,
        note: format!("Top {n} files by edge count shown"),
    }
}

/// Markdown view of a rendered DSM result.
pub fn render_dsm_md(value: &Value) -> String {
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
