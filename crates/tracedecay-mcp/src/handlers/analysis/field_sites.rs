//! `tracedecay_field_sites` — read and write references to a named field.

use super::*;

#[hotpath::measure(future = true, label = "mcp.analysis.field_sites.total")]
pub async fn handle_field_sites(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let raw =
        args.get("field")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "tracedecay_field_sites requires a 'field' argument".to_string(),
            })?;
    let writes_only = args
        .get("writes_only")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(200, |v| v.clamp(1, 2000) as usize);

    let (qualifier, field_name) = match raw.rsplit_once("::") {
        Some((q, f)) => (Some(q.to_string()), f.to_string()),
        None => (None, raw.to_string()),
    };

    let (symbols_by_file, qualified_scope) =
        hotpath::measure_block!("mcp.analysis.field_sites.graph", {
            let symbols = verified_analysis_symbols(graph, scope_prefix)?;
            let qualified_scope = qualifier
                .as_deref()
                .map(|qualifier| qualified_field_scope(graph, &symbols, qualifier, &field_name))
                .transpose()?;
            let mut symbols_by_file = HashMap::<String, Vec<VerifiedAnalysisSymbol>>::new();
            for symbol in symbols {
                symbols_by_file
                    .entry(symbol.path.clone())
                    .or_default()
                    .push(symbol);
            }
            (symbols_by_file, qualified_scope)
        });
    // Graph phase is done. The source walk reads every candidate file, so it
    // belongs on a blocking worker like the sibling analysis scans.
    let project_root = graph.project_root()?.to_path_buf();
    let response_project_root = project_root.clone();
    let (writes, reads, touched) = hotpath::future!(
        tokio::task::spawn_blocking(move || {
            let mut files = symbols_by_file.keys().cloned().collect::<Vec<_>>();
            files.sort();
            let mut writes: Vec<Value> = Vec::new();
            let mut reads: Vec<Value> = Vec::new();
            let mut touched: Vec<String> = Vec::new();

            'outer: for file in &files {
                let abs = project_root.join(file);
                let Ok(source) = tracedecay_runtime_core::sync::read_source_file(&abs) else {
                    continue;
                };

                // Cheap textual pre-filter before any per-file store read. Most
                // files in a repository never mention the field, and fetching their
                // nodes anyway cost one daemon round trip per file in the project —
                // O(store) work to answer a question whose result is a handful of
                // sites.
                let masked = if path_is_rust(file) {
                    tracedecay_code_extraction::source_mask::masked_rust_source_with(
                        &source,
                        tracedecay_code_extraction::source_mask::MaskOptions::CODE_SCAN,
                    )
                } else {
                    source.clone()
                };
                let sites = find_field_references(&masked, &field_name);
                if sites.is_empty() {
                    continue;
                }
                let nodes = symbols_by_file.get(file).map_or(&[][..], Vec::as_slice);
                let receiver_types = if qualified_scope
                    .as_ref()
                    .is_some_and(|scope| scope.target_exists)
                    && path_is_rust(file)
                {
                    rust_field_receiver_types(&source, &field_name)?
                } else {
                    HashMap::new()
                };

                for site in sites {
                    let line_text = line_at(&source, site.byte).unwrap_or("");
                    let enclosing = nodes
                        .iter()
                        .filter(|n| {
                            let line = site.line.saturating_sub(1);
                            n.metadata.start_line <= line && line <= n.end_line()
                        })
                        .min_by_key(|n| n.metadata.line_span);
                    if let Some(scope) = &qualified_scope {
                        if !scope.target_exists {
                            continue;
                        }
                        match enclosing
                            .zip(receiver_types.get(&site.byte))
                            .and_then(|(symbol, receiver_type)| {
                                scope.site_matches(&symbol.occurrence, receiver_type)
                            }) {
                            Some(true) => {}
                            Some(false) => continue,
                            None => {
                                return Err(TraceDecayError::project_route(
                                    "verified-field-qualifier-unavailable",
                                    false,
                                    format!(
                                        "the indexed graph cannot bind field receiver '{}' at {file}:{} to exactly one qualified owner",
                                        receiver_types
                                            .get(&site.byte)
                                            .map_or("<unresolved>", String::as_str),
                                        site.line,
                                    ),
                                ));
                            }
                        }
                    }
                    let enclosing = enclosing.map(|n| n.metadata.qualified_name.clone());
                    let entry = json!({
                        "file": file,
                        "line": site.line,
                        "enclosing": enclosing,
                        "snippet": line_text.trim(),
                    });
                    if !touched.contains(file) {
                        touched.push(file.clone());
                    }
                    match site.kind {
                        FieldRefKind::Write => {
                            writes.push(entry);
                            if writes.len() >= limit && (writes_only || reads.len() >= limit) {
                                break 'outer;
                            }
                        }
                        FieldRefKind::Read => {
                            if writes_only {
                                continue;
                            }
                            reads.push(entry);
                            if reads.len() >= limit && writes.len() >= limit {
                                break 'outer;
                            }
                        }
                    }
                }
            }
            Ok((writes, reads, touched))
        }),
        label = "mcp.analysis.field_sites.scan"
    )
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("tracedecay_field_sites scan failed to join: {join_error}"),
    })??;

    let qualifier_applied = qualifier.is_some();
    let payload = hotpath::measure_block!("mcp.analysis.field_sites.assemble", {
        if writes_only {
            json!({
                "field": raw,
                "qualifier": qualifier,
                "qualifier_applied": qualifier_applied,
                "write_count": writes.len(),
                "write_sites": writes,
            })
        } else {
            json!({
                "field": raw,
                "qualifier": qualifier,
                "qualifier_applied": qualifier_applied,
                "write_count": writes.len(),
                "read_count": reads.len(),
                "write_sites": writes,
                "read_sites": reads,
            })
        }
    });
    Ok(generic_tool_result(
        Some(&response_project_root),
        &args,
        &payload,
        touched,
    ))
}

struct QualifiedFieldScope {
    target_exists: bool,
    selected_owners: HashSet<SymbolOccurrenceId>,
    owner_names: HashMap<SymbolOccurrenceId, String>,
    enclosing_owners: HashMap<SymbolOccurrenceId, HashSet<SymbolOccurrenceId>>,
}

impl QualifiedFieldScope {
    fn site_matches(&self, enclosing: &SymbolOccurrenceId, receiver_type: &str) -> Option<bool> {
        let mut owners = self
            .enclosing_owners
            .get(enclosing)
            .into_iter()
            .flatten()
            .filter(|owner| {
                self.owner_names
                    .get(*owner)
                    .is_some_and(|name| qualified_type_matches(name, receiver_type))
            })
            .collect::<Vec<_>>();
        if owners.is_empty() {
            owners = self
                .owner_names
                .iter()
                .filter(|(_, name)| qualified_type_matches(name, receiver_type))
                .map(|(owner, _)| owner)
                .collect();
        }
        if owners.len() != 1 {
            return None;
        }
        Some(self.selected_owners.contains(owners[0]))
    }
}

fn qualified_field_scope(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    symbols: &[VerifiedAnalysisSymbol],
    qualifier: &str,
    field_name: &str,
) -> Result<QualifiedFieldScope> {
    let edges = verified_analysis_edges(
        graph,
        symbols,
        &[RelationEdgeKindV1::Contains, RelationEdgeKindV1::TypeOf],
    )?;
    let fields = symbols
        .iter()
        .filter(|symbol| {
            matches!(
                symbol.metadata.kind.as_str(),
                "field" | "val_field" | "var_field"
            ) && symbol.metadata.simple_name == field_name
        })
        .collect::<Vec<_>>();
    let field_occurrences = fields
        .iter()
        .map(|field| field.occurrence.clone())
        .collect::<HashSet<_>>();
    let selected_fields = fields
        .iter()
        .filter(|field| qualified_parent_matches(&field.metadata.qualified_name, qualifier))
        .map(|field| field.occurrence.clone())
        .collect::<HashSet<_>>();
    if selected_fields.is_empty() {
        return Ok(QualifiedFieldScope {
            target_exists: false,
            selected_owners: HashSet::new(),
            owner_names: HashMap::new(),
            enclosing_owners: HashMap::new(),
        });
    }

    let all_owners = edges
        .iter()
        .filter(|edge| {
            edge.edge.kind == RelationEdgeKindV1::Contains
                && field_occurrences.contains(&edge.edge.to_occurrence)
        })
        .map(|edge| edge.edge.from_occurrence.clone())
        .collect::<HashSet<_>>();
    let selected_owners = edges
        .iter()
        .filter(|edge| {
            edge.edge.kind == RelationEdgeKindV1::Contains
                && selected_fields.contains(&edge.edge.to_occurrence)
        })
        .map(|edge| edge.edge.from_occurrence.clone())
        .collect::<HashSet<_>>();
    if selected_owners.is_empty() {
        return Err(verified_analysis_unavailable(
            "field-qualifier",
            "the qualified field has no extraction-attested owner relation",
        ));
    }

    let mut enclosing_owners = HashMap::<SymbolOccurrenceId, HashSet<SymbolOccurrenceId>>::new();
    for edge in &edges {
        if edge.edge.kind == RelationEdgeKindV1::TypeOf
            && all_owners.contains(&edge.edge.to_occurrence)
        {
            enclosing_owners
                .entry(edge.edge.from_occurrence.clone())
                .or_default()
                .insert(edge.edge.to_occurrence.clone());
        }
    }
    let owner_names = symbols
        .iter()
        .filter(|symbol| all_owners.contains(&symbol.occurrence))
        .map(|symbol| {
            (
                symbol.occurrence.clone(),
                symbol.metadata.qualified_name.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let owners_by_name = owner_names
        .iter()
        .map(|(occurrence, name)| (name.clone(), occurrence.clone()))
        .collect::<HashMap<_, _>>();
    let impl_owners = symbols
        .iter()
        .filter(|symbol| symbol.metadata.kind == "impl")
        .filter_map(|symbol| {
            owners_by_name
                .get(&symbol.metadata.qualified_name)
                .map(|owner| (symbol.occurrence.clone(), owner.clone()))
        })
        .collect::<HashMap<_, _>>();
    for edge in &edges {
        if edge.edge.kind == RelationEdgeKindV1::Contains
            && let Some(owner) = impl_owners.get(&edge.edge.from_occurrence)
        {
            enclosing_owners
                .entry(edge.edge.to_occurrence.clone())
                .or_default()
                .insert(owner.clone());
        }
    }

    Ok(QualifiedFieldScope {
        target_exists: true,
        selected_owners,
        owner_names,
        enclosing_owners,
    })
}

fn qualified_parent_matches(qualified_name: &str, qualifier: &str) -> bool {
    let Some((parent, _)) = qualified_name.rsplit_once("::") else {
        return false;
    };
    parent == qualifier
        || parent
            .strip_suffix(qualifier)
            .is_some_and(|prefix| prefix.ends_with("::"))
}

fn qualified_type_matches(qualified_name: &str, type_name: &str) -> bool {
    qualified_name == type_name
        || qualified_name
            .strip_suffix(type_name)
            .is_some_and(|prefix| prefix.ends_with("::"))
}

#[cfg(feature = "source-analysis")]
fn rust_field_receiver_types(source: &str, field: &str) -> Result<HashMap<usize, String>> {
    let language = tracedecay_code_extraction::ts_provider::try_language("rust")
        .map_err(|error| verified_analysis_unavailable("field-qualifier", &error))?;
    let tree =
        tracedecay_code_extraction::redundancy::parse_file(source, &language).ok_or_else(|| {
            verified_analysis_unavailable(
                "field-qualifier",
                "Rust field receiver parse returned no tree",
            )
        })?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(verified_analysis_unavailable(
            "field-qualifier",
            "Rust field receiver parse was recovered from syntax errors",
        ));
    }
    let mut receivers = HashMap::new();
    collect_rust_field_receiver_types(root, source, field, &mut receivers);
    Ok(receivers)
}

#[cfg(not(feature = "source-analysis"))]
fn rust_field_receiver_types(_source: &str, _field: &str) -> Result<HashMap<usize, String>> {
    Err(verified_analysis_unavailable(
        "field-qualifier",
        "Rust field receiver parsing is not mounted",
    ))
}

#[cfg(feature = "source-analysis")]
fn collect_rust_field_receiver_types(
    node: tree_sitter::Node<'_>,
    source: &str,
    field: &str,
    receivers: &mut HashMap<usize, String>,
) {
    if node.kind() == "field_expression"
        && let Some(field_node) = node.child_by_field_name("field")
        && node_text(source, field_node) == Some(field)
        && let Some(value) = node.child_by_field_name("value")
        && matches!(value.kind(), "identifier" | "self")
        && let Some(receiver) = node_text(source, value)
        && let Some(owner) = rust_receiver_type(node, source, receiver)
    {
        receivers.insert(field_node.end_byte(), owner);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_field_receiver_types(child, source, field, receivers);
    }
}

#[cfg(feature = "source-analysis")]
fn rust_receiver_type(node: tree_sitter::Node<'_>, source: &str, receiver: &str) -> Option<String> {
    let function = rust_ancestor(node, "function_item")?;
    if receiver == "self" {
        return rust_ancestor(function, "impl_item")?
            .child_by_field_name("type")
            .and_then(|node| rust_simple_type(node, source));
    }

    let parameters = function.child_by_field_name("parameters")?;
    let mut cursor = parameters.walk();
    let parameter_type = parameters
        .named_children(&mut cursor)
        .find_map(|parameter| {
            let pattern = parameter.child_by_field_name("pattern")?;
            (pattern.kind() == "identifier" && node_text(source, pattern) == Some(receiver))
                .then(|| parameter.child_by_field_name("type"))
                .flatten()
                .and_then(|node| rust_simple_type(node, source))
        })?;
    (!rust_receiver_is_shadowed(node, function, source, receiver)).then_some(parameter_type)
}

#[cfg(feature = "source-analysis")]
fn rust_ancestor<'tree>(
    mut node: tree_sitter::Node<'tree>,
    kind: &str,
) -> Option<tree_sitter::Node<'tree>> {
    while let Some(parent) = node.parent() {
        if parent.kind() == kind {
            return Some(parent);
        }
        node = parent;
    }
    None
}

#[cfg(feature = "source-analysis")]
fn rust_receiver_is_shadowed(
    node: tree_sitter::Node<'_>,
    function: tree_sitter::Node<'_>,
    source: &str,
    receiver: &str,
) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        if parent == function {
            return false;
        }
        if parent.kind() == "block" {
            let mut cursor = parent.walk();
            if parent.named_children(&mut cursor).any(|sibling| {
                sibling.end_byte() <= child.start_byte()
                    && sibling.kind() == "let_declaration"
                    && sibling
                        .child_by_field_name("pattern")
                        .is_some_and(|pattern| rust_pattern_binds(pattern, source, receiver))
            }) {
                return true;
            }
        } else {
            let closure_binds = parent.kind() == "closure_expression"
                && parent
                    .child_by_field_name("parameters")
                    .is_some_and(|parameters| rust_pattern_binds(parameters, source, receiver));
            let condition_binds = matches!(parent.kind(), "if_expression" | "while_expression")
                && parent.child_by_field_name(if parent.kind() == "if_expression" {
                    "consequence"
                } else {
                    "body"
                }) == Some(child)
                && parent
                    .child_by_field_name("condition")
                    .is_some_and(|condition| rust_condition_binds(condition, source, receiver));
            let pattern_binds = parent.kind() != "let_declaration"
                && parent
                    .child_by_field_name("pattern")
                    .is_some_and(|pattern| rust_pattern_binds(pattern, source, receiver));
            if closure_binds || condition_binds || pattern_binds {
                return true;
            }
        }
        child = parent;
    }
    true
}

#[cfg(feature = "source-analysis")]
fn rust_condition_binds(node: tree_sitter::Node<'_>, source: &str, name: &str) -> bool {
    if node.kind() == "let_condition" {
        return node
            .child_by_field_name("pattern")
            .is_some_and(|pattern| rust_pattern_binds(pattern, source, name));
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| rust_condition_binds(child, source, name))
}

#[cfg(feature = "source-analysis")]
fn rust_pattern_binds(node: tree_sitter::Node<'_>, source: &str, name: &str) -> bool {
    if node.kind() == "identifier" && node_text(source, node) == Some(name) {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| rust_pattern_binds(child, source, name))
}

#[cfg(feature = "source-analysis")]
fn rust_simple_type(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "scoped_type_identifier" => node_text(source, node).map(str::to_owned),
        "reference_type" => node
            .child_by_field_name("type")
            .and_then(|node| rust_simple_type(node, source)),
        _ => None,
    }
}

#[cfg(feature = "source-analysis")]
fn node_text<'a>(source: &'a str, node: tree_sitter::Node<'_>) -> Option<&'a str> {
    source.get(node.byte_range())
}

#[derive(Debug, Clone, Copy)]
enum FieldRefKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy)]
struct FieldSite {
    byte: usize,
    line: u32,
    kind: FieldRefKind,
}

fn find_field_references(source: &str, field: &str) -> Vec<FieldSite> {
    let bytes = source.as_bytes();
    let needle = format!(".{field}");
    let mut out: Vec<FieldSite> = Vec::new();
    let mut byte = 0usize;
    while let Some(rel) = source[byte..].find(&needle) {
        let dot = byte + rel;
        let name_start = dot + 1;
        let name_end = name_start + field.len();
        let right_ok = !bytes.get(name_end).copied().is_some_and(is_ident_byte);
        if !right_ok {
            byte = name_end;
            continue;
        }
        if line_is_comment(source, dot) {
            byte = name_end;
            continue;
        }

        out.push(FieldSite {
            byte: name_end,
            line: line_number_at(source, dot),
            kind: classify_field_reference(source, name_end),
        });
        byte = name_end;
    }
    out
}

fn classify_field_reference(source: &str, after_name: usize) -> FieldRefKind {
    let bytes = source.as_bytes();
    let mut probe = after_name;
    while let Some(b) = bytes.get(probe) {
        if *b == b' ' || *b == b'\t' {
            probe += 1;
        } else {
            break;
        }
    }

    if let Some(b'\n') = bytes.get(probe).copied() {
        probe += 1;
        while let Some(b) = bytes.get(probe) {
            if *b == b' ' || *b == b'\t' {
                probe += 1;
            } else {
                break;
            }
        }
    }

    let next = bytes.get(probe).copied();
    let next2 = bytes.get(probe + 1).copied();
    match (next, next2) {
        (Some(b'='), Some(b'=' | b'>')) => FieldRefKind::Read,
        (Some(b'='), _) => FieldRefKind::Write,
        (Some(b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^'), Some(b'=')) => {
            FieldRefKind::Write
        }
        (Some(b'<'), Some(b'<')) | (Some(b'>'), Some(b'>')) => {
            if bytes.get(probe + 2).copied() == Some(b'=') {
                FieldRefKind::Write
            } else {
                FieldRefKind::Read
            }
        }
        _ => {
            if has_mut_borrow_prefix(source, after_name.saturating_sub(1)) {
                FieldRefKind::Write
            } else {
                FieldRefKind::Read
            }
        }
    }
}

fn has_mut_borrow_prefix(source: &str, idx: usize) -> bool {
    let bytes = source.as_bytes();
    let mut probe = idx;
    while probe > 0 && (is_ident_byte(bytes[probe]) || matches!(bytes[probe], b'.' | b':' | b'?')) {
        probe -= 1;
    }
    while probe > 0 && bytes[probe].is_ascii_whitespace() {
        probe -= 1;
    }
    if probe < 4 {
        return false;
    }
    let window = &source[probe.saturating_sub(4)..=probe];
    window.ends_with("&mut")
}

fn line_at(source: &str, byte: usize) -> Option<&str> {
    let line_start = source[..byte].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[byte..].find('\n').map_or(source.len(), |i| byte + i);
    source.get(line_start..line_end)
}

fn line_is_comment(source: &str, byte: usize) -> bool {
    let line_start = source[..byte].rfind('\n').map_or(0, |i| i + 1);
    let line = &source[line_start..];
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
}
