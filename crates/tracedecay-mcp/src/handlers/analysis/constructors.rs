//! `tracedecay_constructors` — struct-literal construction sites and the fields each one sets.

use super::*;
use tree_sitter::{Node, Parser};

#[hotpath::measure(future = true, label = "mcp.analysis.constructors.total")]
pub async fn handle_constructors(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let struct_name =
        args.get("struct")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "tracedecay_constructors requires a 'struct' argument".to_string(),
            })?;
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(100, |v| v.clamp(1, 1000) as usize);

    let struct_nodes = hotpath::measure_block!("mcp.analysis.constructors.resolve", {
        let candidates = graph.resolve_simple_name(struct_name, None, 50)?;
        candidates
            .into_iter()
            .map(|symbol| {
                let metadata = symbol.metadata.as_ref().ok_or_else(|| {
                    TraceDecayError::project_route(
                        "code-graph-corrupt",
                        false,
                        "constructor candidate is missing extraction-attested metadata",
                    )
                })?;
                let is_container =
                    matches!(metadata.kind.as_str(), "struct" | "class" | "case_class");
                Ok(is_container.then_some(symbol))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
    });

    if struct_nodes.is_empty() {
        let payload = hotpath::measure_block!(
            "mcp.analysis.constructors.assemble",
            json!({
                "found": false,
                "struct": struct_name,
                "message": format!("No struct, class, or case-class named '{struct_name}' found."),
                "match_count": 0,
                "sites": [],
            })
        );
        return Ok(generic_tool_result(
            Some(graph.project_root()?),
            &args,
            &payload,
            vec![],
        ));
    }
    let candidate_count = struct_nodes.len();
    let ambiguous_definition = candidate_count != 1;

    let (expected_fields, files) = hotpath::measure_block!("mcp.analysis.constructors.graph", {
        let mut expected_fields: HashSet<String> = HashSet::new();
        let seeds = struct_nodes
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        for children in graph.callees(&seeds, &[RelationEdgeKindV1::Contains], 10_000)? {
            for child in children {
                let metadata = child.neighbor.metadata.ok_or_else(|| {
                    TraceDecayError::project_route(
                        "code-graph-corrupt",
                        false,
                        "constructor field relation is missing extraction-attested metadata",
                    )
                })?;
                if matches!(metadata.kind.as_str(), "field" | "val_field" | "var_field") {
                    expected_fields.insert(metadata.simple_name);
                }
            }
        }
        let files = verified_analysis_symbols(graph, scope_prefix)?
            .into_iter()
            .map(|symbol| symbol.path)
            .collect::<HashSet<_>>();
        (expected_fields, files)
    });
    let project_root = graph.project_root()?;
    let mut reported_expected_fields = expected_fields.iter().cloned().collect::<Vec<_>>();
    reported_expected_fields.sort();

    // Reading and parsing every source file in the project is a long CPU and
    // I/O slice with no await points. Running it inline pinned a request
    // runtime worker for the whole scan — tens of seconds on a large
    // repository — which is exactly what starves other interactive calls. The
    // scan is self-contained, so it belongs on a blocking thread.
    let mut scan_paths = files.into_iter().collect::<Vec<_>>();
    scan_paths.sort();
    let scan_root = project_root.to_path_buf();
    let scan_struct = struct_name.to_string();
    let scan_fields = expected_fields.clone();
    let scan_ambiguous_definition = ambiguous_definition;

    let (sites, touched) = hotpath::future!(
        tokio::task::spawn_blocking(move || -> Result<_> {
            let mut sites: Vec<Value> = Vec::new();
            let mut touched: Vec<String> = Vec::new();
            let language = tracedecay_code_extraction::ts_provider::try_language("rust")
                .map_err(|message| TraceDecayError::Config { message })?;
            let mut parser = Parser::new();
            parser
                .set_language(&language)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("configure Rust constructor parser: {error}"),
                })?;

            'outer: for path in &scan_paths {
                if !path_is_rust(path) {
                    continue;
                }
                let abs = scan_root.join(path);
                let source =
                    tracedecay_runtime_core::sync::read_source_file(&abs).map_err(|error| {
                        TraceDecayError::File {
                            message: format!("read indexed constructor source: {error}"),
                            path: path.clone(),
                        }
                    })?;

                for site in find_struct_literals(&mut parser, &source, &scan_struct)? {
                    let mut omitted: Vec<String> = if scan_fields.is_empty() {
                        Vec::new()
                    } else {
                        scan_fields
                            .iter()
                            .filter(|field| !site.fields.contains(field))
                            .cloned()
                            .collect()
                    };
                    omitted.sort();
                    let coverage_unknown = scan_ambiguous_definition || site.has_syntax_errors;
                    let (update_fields, missing_fields) = if coverage_unknown {
                        (Vec::new(), Vec::new())
                    } else if site.has_update {
                        (omitted, Vec::new())
                    } else {
                        (Vec::new(), omitted)
                    };
                    if !touched.contains(path) {
                        touched.push(path.clone());
                    }
                    sites.push(json!({
                        "file": path,
                        "line": site.line,
                        "fields": site.fields,
                        "update_fields": update_fields,
                        "missing_fields": missing_fields,
                        "field_coverage": if coverage_unknown { "unknown" } else { "complete" },
                    }));
                    if sites.len() >= limit {
                        break 'outer;
                    }
                }
            }

            Ok((sites, touched))
        }),
        label = "mcp.analysis.constructors.scan"
    )
    .await
    .map_err(|e| TraceDecayError::Config {
        message: format!("tracedecay_constructors scan failed to join: {e}"),
    })??;

    let payload = hotpath::measure_block!(
        "mcp.analysis.constructors.assemble",
        json!({
            "struct": struct_name,
            "candidate_count": candidate_count,
            "resolution_status": "unverified",
            "resolution_reason": if ambiguous_definition {
                "ambiguous_simple_name"
            } else {
                "syntax_only_simple_name"
            },
            "expected_fields": if ambiguous_definition {
                Value::Null
            } else {
                json!(reported_expected_fields)
            },
            "match_count": sites.len(),
            "sites": sites,
        })
    );
    Ok(generic_tool_result(
        Some(project_root),
        &args,
        &payload,
        touched,
    ))
}

struct LiteralSite {
    line: u32,
    fields: Vec<String>,
    has_update: bool,
    has_syntax_errors: bool,
}

fn find_struct_literals(
    parser: &mut Parser,
    source: &str,
    struct_name: &str,
) -> Result<Vec<LiteralSite>> {
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| TraceDecayError::Config {
            message: "Rust constructor parse was cancelled".to_string(),
        })?;
    let mut sites = Vec::new();
    collect_struct_literals(tree.root_node(), source, struct_name, &mut sites);
    Ok(sites)
}

fn collect_struct_literals(
    node: Node<'_>,
    source: &str,
    struct_name: &str,
    sites: &mut Vec<LiteralSite>,
) {
    if node.kind() == "struct_expression"
        && node
            .child_by_field_name("name")
            .and_then(simple_type_name)
            .and_then(|name| name.utf8_text(source.as_bytes()).ok())
            == Some(struct_name)
        && let Some(body) = node.child_by_field_name("body")
    {
        let mut fields = Vec::new();
        let mut has_update = false;
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            match child.kind() {
                "field_initializer" => {
                    if let Some(field) = child
                        .child_by_field_name("field")
                        .and_then(|field| field.utf8_text(source.as_bytes()).ok())
                    {
                        fields.push(field.to_string());
                    }
                }
                "shorthand_field_initializer" => {
                    if let Ok(field) = child.utf8_text(source.as_bytes()) {
                        fields.push(field.to_string());
                    }
                }
                "base_field_initializer" => has_update = true,
                _ => {}
            }
        }
        sites.push(LiteralSite {
            line: node.start_position().row as u32 + 1,
            fields,
            has_update,
            has_syntax_errors: node.has_error(),
        });
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_struct_literals(child, source, struct_name, sites);
    }
}

fn simple_type_name(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        if node.kind() == "type_identifier" {
            return Some(node);
        }
        node = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("type"))?;
    }
}
