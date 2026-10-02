/// Tree-sitter based Go source code extractor.
///
/// Parses Go source files and emits nodes and edges for the code graph.
use std::time::Instant;

use tree_sitter::{Node as TsNode, Tree};

use crate::common::{
    ExtractionState, clean_c_comment, docstring_from_preceding_comments, local_node_id,
};
use crate::complexity::{GO_COMPLEXITY, count_complexity};
use crate::extraction_artifact::{
    ExtractedGoMethodSetRowV1, ExtractedImportEvidenceV1, ExtractionArtifactV1, GoMethodSetRowV1,
    GoMethodSignatureV1, GoTypeTokenV1, GoTypeV1, ImportBindingV1, ImportNamespaceV1,
};
use crate::traversal::find_direct_child_by_kind;
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, Node, NodeKind, UnresolvedRef, Visibility,
    generate_node_id,
};

/// Extracts code graph nodes and edges from Go source files using tree-sitter.
pub struct GoExtractor;

impl GoExtractor {
    fn extract_tree(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        let start = Instant::now();
        let mut state = ExtractionState::new(file_path, source);
        let mut imports = Vec::new();
        let mut method_sets = Vec::new();

        let file_node = Node {
            id: generate_node_id(file_path, &NodeKind::File, file_path, 0),
            kind: NodeKind::File,
            name: file_path.to_string(),
            qualified_name: file_path.to_string(),
            file_path: file_path.to_string(),
            start_line: 0,
            attrs_start_line: 0,
            end_line: crate::common::file_end_line(source, tree),
            start_column: 0,
            end_column: 0,
            signature: None,
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        let file_node_id = file_node.id.clone();
        state.nodes.push(file_node);
        state.node_stack.push((file_path.to_string(), file_node_id));

        let metrics = if file_path.ends_with(".mod") {
            Self::visit_module_manifest(&mut state, source);
            crate::parsed_extraction::ParsedTraversalMetrics::default()
        } else {
            crate::parsed_extraction::visit_root_children(tree, scope, |child| {
                Self::visit_node(&mut state, &mut method_sets, child);
                if child.kind() == "import_declaration" {
                    Self::import_evidence(&mut state, &mut imports, child);
                }
            })
        };

        state.node_stack.pop();

        let mut artifact = ExtractionArtifactV1::with_imports(state.into_result(start), imports);
        artifact.go_method_sets = method_sets;
        crate::parsed_extraction::ParsedExtractionArtifactV1::complete(artifact, scope, metrics)
    }

    /// The `module` directive of a `go.mod` manifest becomes a `Module` symbol
    /// named by the module path, the prefix every import of the module's
    /// packages starts with.
    fn visit_module_manifest(state: &mut ExtractionState, source: &str) {
        // ponytail: `.mod` dispatches by extension, so a non-Go `.mod` file
        // indexes here as a bare Go file; a file-name keyed dispatch would
        // route only `go.mod`.
        if state.file_path.rsplit('/').next() != Some("go.mod") {
            return;
        }
        let Some((line, text, path)) = source.lines().enumerate().find_map(|(line, text)| {
            let directive = text.split("//").next().unwrap_or(text).trim();
            let path = directive.strip_prefix("module")?;
            if !path.starts_with(char::is_whitespace) {
                return None;
            }
            let path = path.trim().trim_matches('"');
            (!path.is_empty()).then_some((line, text, path))
        }) else {
            return;
        };
        let (Ok(line), Ok(end_column)) = (u32::try_from(line), u32::try_from(text.len())) else {
            state
                .errors
                .push("go.mod module directive exceeds the canonical position width".to_owned());
            return;
        };
        let id = generate_node_id(&state.file_path, &NodeKind::Module, path, line);
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::Module,
            name: path.to_owned(),
            qualified_name: format!("{}::{path}", state.qualified_prefix()),
            file_path: state.file_path.clone(),
            start_line: line,
            attrs_start_line: line,
            end_line: line,
            start_column: 0,
            end_column,
            signature: Some(text.trim().to_owned()),
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        });
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(line),
            });
        }
    }

    /// One import row per spec: a package bound under its alias or its
    /// default name, a dot import as a glob, and a blank import as a load.
    fn import_evidence(
        state: &mut ExtractionState,
        imports: &mut Vec<ExtractedImportEvidenceV1>,
        declaration: TsNode<'_>,
    ) {
        let mut specs = Vec::new();
        let mut cursor = declaration.walk();
        for child in declaration.named_children(&mut cursor) {
            match child.kind() {
                "import_spec" => specs.push(child),
                "import_spec_list" => {
                    let mut inner = child.walk();
                    specs.extend(
                        child
                            .named_children(&mut inner)
                            .filter(|spec| spec.kind() == "import_spec"),
                    );
                }
                _ => {}
            }
        }
        for spec in specs {
            let Some(path) = spec.child_by_field_name("path") else {
                continue;
            };
            let path = state.node_text(path).trim_matches(|c| c == '"' || c == '`');
            if path.is_empty() {
                continue;
            }
            let alias = spec
                .child_by_field_name("name")
                .map(|name| state.node_text(name));
            let binding = match alias {
                Some(".") => ImportBindingV1::Glob,
                Some("_") => ImportBindingV1::SideEffect,
                Some(local) => ImportBindingV1::Namespace { local },
                None => ImportBindingV1::Namespace {
                    local: go_default_package_name(path),
                },
            };
            match ExtractedImportEvidenceV1::private_binding(
                &state.file_path,
                "go",
                path,
                binding,
                ImportNamespaceV1::Value,
                spec,
            ) {
                Ok(row) => imports.push(row),
                Err(error) => state.errors.push(error),
            }
        }
    }

    fn visit_node(
        state: &mut ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        node: TsNode<'_>,
    ) {
        match node.kind() {
            "package_clause" => Self::visit_package(state, node),
            "import_declaration" => Self::visit_imports(state, node),
            "function_declaration" => Self::visit_function(state, node),
            "method_declaration" => Self::visit_method(state, method_sets, node),
            "type_declaration" => Self::visit_type_declaration(state, method_sets, node),
            "const_declaration" => Self::visit_const_declaration(state, node),
            "var_declaration" => Self::visit_var_declaration(state, node),
            // Comments are picked up as docstrings by the definitions they precede.
            _ => {}
        }
    }

    /// Extract a package clause node.
    fn visit_package(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "package_identifier").map_or_else(
            || "<unknown>".to_string(),
            |n| state.node_text(n).to_string(),
        );
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::GoPackage,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::GoPackage,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(state.node_text(node).to_string()),
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent (File).
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract import declarations. Each import spec becomes a Use node.
    fn visit_imports(state: &mut ExtractionState, node: TsNode<'_>) {
        // Imports can be: import "foo" or import ( "foo"; "bar" )
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "import_spec" => {
                        Self::visit_single_import(state, child);
                    }
                    "import_spec_list" => {
                        // Walk into the spec list to find individual import_spec nodes.
                        let mut inner = child.walk();
                        if inner.goto_first_child() {
                            loop {
                                let spec = inner.node();
                                if spec.kind() == "import_spec" {
                                    Self::visit_single_import(state, spec);
                                }
                                if !inner.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract a single import spec as a Use node.
    fn visit_single_import(state: &mut ExtractionState, node: TsNode<'_>) {
        let text = state.node_text(node);
        // Strip quotes from the import path.
        let path = text.trim().trim_matches('"').to_string();
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), path);
        let id = local_node_id(&state.file_path, state.source, &NodeKind::Use, &path, node);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Use,
            name: path.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
            docstring: None,
            visibility: Visibility::Private,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent (File).
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        // Unresolved Uses reference.
        state.unresolved_refs.push(UnresolvedRef {
            from_node_id: id,
            reference_name: path,
            reference_kind: EdgeKind::Uses,
            line: start_line,
            column: start_column,
            file_path: state.file_path.clone(),
            unmodeled_import: None,
            argument_count: None,
        });
    }

    /// Extract a function declaration node.
    fn visit_function(state: &mut ExtractionState, node: TsNode<'_>) {
        // In Go, function name is an `identifier` child.
        let name = find_direct_child_by_kind(node, "identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );
        let visibility = Self::go_visibility(&name);
        let signature = Some(Self::extract_signature(state, node));
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Function,
            &name,
            node,
        );
        let metrics = count_complexity(node, &GO_COMPLEXITY, state.source);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Function,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
            docstring,
            visibility,
            is_async: false,
            branches: metrics.branches,
            loops: metrics.loops,
            returns: metrics.returns,
            max_nesting: metrics.max_nesting,
            unsafe_blocks: metrics.unsafe_blocks,
            unchecked_calls: metrics.unchecked_calls,
            assertions: metrics.assertions,
            complexity_analysis: metrics.analysis,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        Self::extract_type_params(state, node, &id);

        if let Some(body) = find_direct_child_by_kind(node, "block") {
            Self::extract_call_sites(state, body, &id);
        }
    }

    /// Extract a method declaration node (function with receiver).
    fn visit_method(
        state: &mut ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        node: TsNode<'_>,
    ) {
        // In Go, method name is a `field_identifier` child.
        let name = find_direct_child_by_kind(node, "field_identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );
        let visibility = Self::go_visibility(&name);
        let signature = Some(Self::extract_signature(state, node));
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::StructMethod,
            &name,
            node,
        );
        let metrics = count_complexity(node, &GO_COMPLEXITY, state.source);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::StructMethod,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
            docstring,
            visibility,
            is_async: false,
            branches: metrics.branches,
            loops: metrics.loops,
            returns: metrics.returns,
            max_nesting: metrics.max_nesting,
            unsafe_blocks: metrics.unsafe_blocks,
            unchecked_calls: metrics.unchecked_calls,
            assertions: metrics.assertions,
            complexity_analysis: metrics.analysis,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent (File).
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        if let Some(type_name) = Self::extract_receiver(state, node, &id) {
            method_sets.push(ExtractedGoMethodSetRowV1 {
                node_id: id.clone(),
                row: GoMethodSetRowV1::Receiver {
                    type_name,
                    method: Self::method_signature(state, node),
                },
            });
        }

        if let Some(body) = find_direct_child_by_kind(node, "block") {
            Self::extract_call_sites(state, body, &id);
        }
    }

    /// Extract a type declaration (struct, interface, or type alias).
    fn visit_type_declaration(
        state: &mut ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        node: TsNode<'_>,
    ) {
        // A type_declaration contains either a type_spec or a type_alias child.
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "type_spec" => Self::visit_type_spec(state, method_sets, child, node),
                    "type_alias" => Self::visit_type_alias(state, method_sets, child, node),
                    _ => {}
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract a `type_spec` node, dispatching on whether it defines a struct or interface.
    fn visit_type_spec(
        state: &mut ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        spec_node: TsNode<'_>,
        decl_node: TsNode<'_>,
    ) {
        let name = find_direct_child_by_kind(spec_node, "type_identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );

        // Check what type is being defined.
        if let Some(struct_type) = find_direct_child_by_kind(spec_node, "struct_type") {
            Self::visit_struct(state, method_sets, &name, struct_type, decl_node);
        } else if let Some(iface_type) = find_direct_child_by_kind(spec_node, "interface_type") {
            Self::visit_interface(state, method_sets, &name, spec_node, iface_type, decl_node);
        } else {
            // A plain type definition (e.g., `type Foo int`) that is not a type alias.
            // Treat it like a type alias for graph purposes.
            Self::visit_named_type(state, method_sets, &name, decl_node);
        }
    }

    /// Extract a struct type definition.
    fn visit_struct(
        state: &mut ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        name: &str,
        struct_type: TsNode<'_>,
        decl_node: TsNode<'_>,
    ) {
        let visibility = Self::go_visibility(name);
        let docstring = Self::extract_docstring(state, decl_node);
        let text = state.node_text(decl_node);
        let signature = text.find('{').map(|pos| text[..pos].trim().to_string());
        let start_line = decl_node.start_position().row as u32;
        let end_line = decl_node.end_position().row as u32;
        let start_column = decl_node.start_position().column as u32;
        let end_column = decl_node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Struct,
            name,
            decl_node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Struct,
            name: name.to_string(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
            docstring,
            visibility,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        method_sets.push(ExtractedGoMethodSetRowV1 {
            node_id: id.clone(),
            row: GoMethodSetRowV1::NamedType,
        });
        state.node_stack.push((name.to_string(), id.clone()));
        Self::extract_struct_fields(state, struct_type);
        state.node_stack.pop();
    }

    /// Extract fields from a `struct_type` node.
    fn extract_struct_fields(state: &mut ExtractionState, struct_type: TsNode<'_>) {
        if let Some(field_list) = find_direct_child_by_kind(struct_type, "field_declaration_list") {
            let mut cursor = field_list.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "field_declaration" {
                        Self::extract_single_field(state, child);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }

    /// Extract a single field from a `field_declaration` node.
    fn extract_single_field(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "field_identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );
        let visibility = Self::go_visibility(&name);
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Field,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Field,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
            docstring: None,
            visibility,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent (the struct).
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        // Extract struct tags (raw_string_literal in field_declaration).
        if let Some(tag_node) = find_direct_child_by_kind(node, "raw_string_literal") {
            Self::extract_struct_tag(state, tag_node, &name, &id);
        }
    }

    /// Extract a struct tag from a `raw_string_literal` node.
    fn extract_struct_tag(
        state: &mut ExtractionState,
        tag_node: TsNode<'_>,
        field_name: &str,
        field_id: &str,
    ) {
        let tag_text = state.node_text(tag_node);
        let start_line = tag_node.start_position().row as u32;
        let end_line = tag_node.end_position().row as u32;
        let start_column = tag_node.start_position().column as u32;
        let end_column = tag_node.end_position().column as u32;
        let tag_name = format!("{field_name}:tag");
        let qualified_name = format!("{}::{}", state.qualified_prefix(), tag_name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::StructTag,
            &tag_name,
            tag_node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::StructTag,
            name: tag_name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(tag_text.to_string()),
            docstring: None,
            visibility: Visibility::Private,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from field.
        state.edges.push(Edge {
            source: field_id.to_string(),
            target: id,
            kind: EdgeKind::Contains,
            line: Some(start_line),
        });
    }

    /// Extract an interface type definition.
    fn visit_interface(
        state: &mut ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        name: &str,
        spec_node: TsNode<'_>,
        iface_type: TsNode<'_>,
        decl_node: TsNode<'_>,
    ) {
        let visibility = Self::go_visibility(name);
        let docstring = Self::extract_docstring(state, decl_node);
        let text = state.node_text(decl_node);
        let signature = text.find('{').map(|pos| text[..pos].trim().to_string());
        let start_line = decl_node.start_position().row as u32;
        let end_line = decl_node.end_position().row as u32;
        let start_column = decl_node.start_position().column as u32;
        let end_column = decl_node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::InterfaceType,
            name,
            decl_node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::InterfaceType,
            name: name.to_string(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
            docstring,
            visibility,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        // Extract embedded interfaces (type_elem children).
        Self::extract_interface_embeddings(state, iface_type, &id);
        Self::interface_method_set(state, method_sets, spec_node, iface_type, &id);
        state.node_stack.push((name.to_string(), id));
        Self::extract_interface_methods(state, iface_type);
        state.node_stack.pop();
    }

    /// The method-set rows of an interface body, all keyed by the interface.
    /// A generic interface records only that it is generic: its method
    /// signatures mention type parameters no named type's methods share.
    fn interface_method_set(
        state: &ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        spec_node: TsNode<'_>,
        iface_type: TsNode<'_>,
        iface_id: &str,
    ) {
        let row = |row| ExtractedGoMethodSetRowV1 {
            node_id: iface_id.to_owned(),
            row,
        };
        if spec_node.child_by_field_name("type_parameters").is_some() {
            method_sets.push(row(GoMethodSetRowV1::GenericInterface));
            return;
        }
        let mut cursor = iface_type.walk();
        for child in iface_type.named_children(&mut cursor) {
            match child.kind() {
                "method_elem" => method_sets.push(row(GoMethodSetRowV1::InterfaceMethod {
                    method: Self::method_signature(state, child),
                })),
                "type_elem" => method_sets.push(row(GoMethodSetRowV1::Embeds {
                    embedded: Self::type_tokens(state, child),
                })),
                _ => {}
            }
        }
    }

    /// Extract the `method_elem` children of an `interface_type`.
    fn extract_interface_methods(state: &mut ExtractionState, iface_type: TsNode<'_>) {
        let mut cursor = iface_type.walk();
        for child in iface_type.named_children(&mut cursor) {
            if child.kind() == "method_elem" {
                Self::visit_interface_method(state, child);
            }
        }
    }

    /// Extract a `method_elem` of an interface as a bodiless method the
    /// interface contains.
    fn visit_interface_method(state: &mut ExtractionState, node: TsNode<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = state.node_text(name_node).to_string();
        let visibility = Self::go_visibility(&name);
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::AbstractMethod,
            &name,
            node,
        );
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::AbstractMethod,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line: node.end_position().row as u32,
            start_column: node.start_position().column as u32,
            end_column: node.end_position().column as u32,
            signature: Some(state.node_text(node).trim().to_string()),
            docstring,
            visibility,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        });
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract embedded interface types from an `interface_type` node.
    fn extract_interface_embeddings(
        state: &mut ExtractionState,
        iface_type: TsNode<'_>,
        iface_id: &str,
    ) {
        let mut cursor = iface_type.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "type_elem" {
                    // type_elem contains a type_identifier for the embedded interface.
                    if let Some(type_id) = find_direct_child_by_kind(child, "type_identifier") {
                        let embedded_name = state.node_text(type_id);
                        let line = child.start_position().row as u32;
                        let column = child.start_position().column as u32;
                        state.unresolved_refs.push(UnresolvedRef {
                            from_node_id: iface_id.to_string(),
                            reference_name: embedded_name.to_string(),
                            reference_kind: EdgeKind::Extends,
                            line,
                            column,
                            file_path: state.file_path.clone(),
                            unmodeled_import: None,
                            argument_count: None,
                        });
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract a type alias (e.g., `type StringSlice = []string`).
    fn visit_type_alias(
        state: &mut ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        alias_node: TsNode<'_>,
        decl_node: TsNode<'_>,
    ) {
        let name = find_direct_child_by_kind(alias_node, "type_identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );
        let visibility = Self::go_visibility(&name);
        let docstring = Self::extract_docstring(state, decl_node);
        let text = state.node_text(decl_node);
        let start_line = decl_node.start_position().row as u32;
        let end_line = decl_node.end_position().row as u32;
        let start_column = decl_node.start_position().column as u32;
        let end_column = decl_node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::TypeAlias,
            &name,
            decl_node,
        );
        method_sets.push(ExtractedGoMethodSetRowV1 {
            node_id: id.clone(),
            row: GoMethodSetRowV1::NamedType,
        });

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::TypeAlias,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
            docstring,
            visibility,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract a named type definition that is neither struct nor interface.
    fn visit_named_type(
        state: &mut ExtractionState,
        method_sets: &mut Vec<ExtractedGoMethodSetRowV1>,
        name: &str,
        decl_node: TsNode<'_>,
    ) {
        let visibility = Self::go_visibility(name);
        let docstring = Self::extract_docstring(state, decl_node);
        let text = state.node_text(decl_node);
        let start_line = decl_node.start_position().row as u32;
        let end_line = decl_node.end_position().row as u32;
        let start_column = decl_node.start_position().column as u32;
        let end_column = decl_node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::TypeAlias,
            name,
            decl_node,
        );
        method_sets.push(ExtractedGoMethodSetRowV1 {
            node_id: id.clone(),
            row: GoMethodSetRowV1::NamedType,
        });

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::TypeAlias,
            name: name.to_string(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
            docstring,
            visibility,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract a const declaration. May contain multiple `const_spec` children.
    fn visit_const_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "const_spec" {
                    Self::visit_const_spec(state, child);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract a single const spec.
    fn visit_const_spec(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );
        let visibility = Self::go_visibility(&name);
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Const,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Const,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
            docstring: None,
            visibility,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract a var declaration. May contain multiple `var_spec` children.
    fn visit_var_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "var_spec" => Self::visit_var_spec(state, child),
                    // `var ( … )` groups its specs in a list.
                    "var_spec_list" => Self::visit_var_declaration(state, child),
                    _ => {}
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract a single var spec as a Static node (Go vars are package-level state).
    fn visit_var_spec(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );
        let visibility = Self::go_visibility(&name);
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Static,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Static,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
            docstring: None,
            visibility,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
        // `var registry = build()` runs at package initialization; the
        // variable owns its initializer's calls.
        Self::extract_call_sites(state, node, &id);
    }

    /// Extract the receiver type from a `method_declaration`, create a
    /// Receives edge, and return the receiver's type name.
    fn extract_receiver(
        state: &mut ExtractionState,
        node: TsNode<'_>,
        method_id: &str,
    ) -> Option<String> {
        let mut receiver = None;
        // The first parameter_list child is the receiver.
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "parameter_list" {
                    // This is the receiver parameter list.
                    // Extract the type name from the parameter_declaration inside.
                    if let Some(param) = find_direct_child_by_kind(child, "parameter_declaration") {
                        receiver = Self::extract_receiver_type_name(state, param);
                        if let Some(type_name) = receiver.clone() {
                            let line = child.start_position().row as u32;
                            let column = child.start_position().column as u32;
                            // Create an unresolved Receives reference.
                            state.unresolved_refs.push(UnresolvedRef {
                                from_node_id: method_id.to_string(),
                                reference_name: type_name.clone(),
                                reference_kind: EdgeKind::Receives,
                                line,
                                column,
                                file_path: state.file_path.clone(),
                                unmodeled_import: None,
                                argument_count: None,
                            });
                            // Also try to create a direct Receives edge if we can find
                            // the struct node. We look for it by matching name.
                            let struct_id = state
                                .nodes
                                .iter()
                                .find(|n| n.kind == NodeKind::Struct && n.name == type_name)
                                .map(|n| n.id.clone());
                            if let Some(struct_id) = struct_id {
                                state.edges.push(Edge {
                                    source: method_id.to_string(),
                                    target: struct_id,
                                    kind: EdgeKind::Receives,
                                    line: Some(line),
                                });
                            }
                        }
                    }
                    break;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        receiver
    }

    /// Extract the type name from a receiver `parameter_declaration`:
    /// `c Circle`, `c *Circle`, `l List[T]`, and `l *List[T]`.
    fn extract_receiver_type_name(state: &ExtractionState, param: TsNode<'_>) -> Option<String> {
        let mut ty = param.child_by_field_name("type")?;
        if ty.kind() == "pointer_type" {
            ty = ty.named_child(0)?;
        }
        if ty.kind() == "generic_type" {
            ty = ty.child_by_field_name("type")?;
        }
        (ty.kind() == "type_identifier").then(|| state.node_text(ty).to_string())
    }

    /// The name, parameter types, and result types of a `method_declaration`
    /// or `method_elem`, with parameter names dropped and grouped
    /// parameters expanded.
    fn method_signature(state: &ExtractionState, node: TsNode<'_>) -> GoMethodSignatureV1 {
        let name = node
            .child_by_field_name("name")
            .map(|name| state.node_text(name).to_string())
            .unwrap_or_default();
        let params = node
            .child_by_field_name("parameters")
            .map(|list| Self::parameter_types(state, list))
            .unwrap_or_default();
        let results = match node.child_by_field_name("result") {
            Some(list) if list.kind() == "parameter_list" => Self::parameter_types(state, list),
            Some(result) => vec![Self::type_tokens(state, result)],
            None => Vec::new(),
        };
        GoMethodSignatureV1 {
            name,
            params,
            results,
        }
    }

    /// One type per parameter of a `parameter_list`: `a, b int` is two
    /// `int`s, `xs ...T` is `...` followed by `T`.
    fn parameter_types(state: &ExtractionState, list: TsNode<'_>) -> Vec<GoTypeV1> {
        let mut types = Vec::new();
        let mut cursor = list.walk();
        for param in list.named_children(&mut cursor) {
            let Some(ty) = param.child_by_field_name("type") else {
                continue;
            };
            let mut tokens = Vec::new();
            if param.kind() == "variadic_parameter_declaration" {
                tokens.push(GoTypeTokenV1::Text("...".to_owned()));
            }
            tokens.extend(Self::type_tokens(state, ty));
            let mut names = param.walk();
            let count = param
                .children_by_field_name("name", &mut names)
                .count()
                .max(1);
            types.extend(std::iter::repeat_n(tokens, count));
        }
        types
    }

    /// The tokens of a type node. Identifiers that name types are `Local` or
    /// `Qualified` so the seal can qualify them; every other leaf is `Text`.
    fn type_tokens(state: &ExtractionState, node: TsNode<'_>) -> GoTypeV1 {
        let mut tokens = Vec::new();
        Self::push_type_tokens(state, node, &mut tokens);
        tokens
    }

    fn push_type_tokens(state: &ExtractionState, node: TsNode<'_>, tokens: &mut GoTypeV1) {
        let text = |node: TsNode<'_>| state.node_text(node).to_string();
        match node.kind() {
            "comment" | ";" => {}
            "type_identifier" if is_predeclared_type(state.node_text(node)) => {
                tokens.push(GoTypeTokenV1::Text(text(node)));
            }
            "type_identifier" => tokens.push(GoTypeTokenV1::Local(text(node))),
            "qualified_type" => {
                if let (Some(package), Some(name)) = (
                    node.child_by_field_name("package"),
                    node.child_by_field_name("name"),
                ) {
                    tokens.push(GoTypeTokenV1::Qualified {
                        package: text(package),
                        name: text(name),
                    });
                }
            }
            "parenthesized_type" => {
                if let Some(inner) = node.named_child(0) {
                    Self::push_type_tokens(state, inner, tokens);
                }
            }
            "parameter_list" => {
                tokens.push(GoTypeTokenV1::Text("(".to_owned()));
                for (index, param) in Self::parameter_types(state, node).into_iter().enumerate() {
                    if index > 0 {
                        tokens.push(GoTypeTokenV1::Text(",".to_owned()));
                    }
                    tokens.extend(param);
                }
                tokens.push(GoTypeTokenV1::Text(")".to_owned()));
            }
            "function_type" | "method_elem" => {
                let result = node.child_by_field_name("result");
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    // `func() T` and `func() (T)` are one type.
                    if Some(child) == result && child.kind() != "parameter_list" {
                        tokens.push(GoTypeTokenV1::Text("(".to_owned()));
                        Self::push_type_tokens(state, child, tokens);
                        tokens.push(GoTypeTokenV1::Text(")".to_owned()));
                    } else {
                        Self::push_type_tokens(state, child, tokens);
                    }
                }
            }
            _ if node.child_count() == 0 => tokens.push(GoTypeTokenV1::Text(text(node))),
            _ => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    Self::push_type_tokens(state, child, tokens);
                }
            }
        }
    }

    /// Extract type parameters (generics) from a function or method declaration.
    fn extract_type_params(state: &mut ExtractionState, node: TsNode<'_>, parent_id: &str) {
        if let Some(type_params) = find_direct_child_by_kind(node, "type_parameter_list") {
            let mut cursor = type_params.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "type_parameter_declaration" {
                        // Each type_parameter_declaration has an identifier for the param name.
                        if let Some(ident) = find_direct_child_by_kind(child, "identifier") {
                            let name = state.node_text(ident);
                            let start_line = child.start_position().row as u32;
                            let end_line = child.end_position().row as u32;
                            let start_column = child.start_position().column as u32;
                            let end_column = child.end_position().column as u32;
                            let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
                            let id = local_node_id(
                                &state.file_path,
                                state.source,
                                &NodeKind::GenericParam,
                                name,
                                child,
                            );
                            let text = state.node_text(child);

                            let graph_node = Node {
                                id: id.clone(),
                                kind: NodeKind::GenericParam,
                                name: name.to_string(),
                                qualified_name,
                                file_path: state.file_path.clone(),
                                start_line,
                                attrs_start_line: start_line,
                                end_line,
                                start_column,
                                end_column,
                                signature: Some(text.trim().to_string()),
                                docstring: None,
                                visibility: Visibility::Private,
                                is_async: false,
                                branches: 0,
                                loops: 0,
                                returns: 0,
                                max_nesting: 0,
                                unsafe_blocks: 0,
                                unchecked_calls: 0,
                                assertions: 0,
                                complexity_analysis: ComplexityAnalysisV1::Complete,
                                updated_at: state.timestamp,
                                parent_id: None,
                            };
                            state.nodes.push(graph_node);

                            // Contains edge from the function/method.
                            state.edges.push(Edge {
                                source: parent_id.to_string(),
                                target: id,
                                kind: EdgeKind::Contains,
                                line: Some(start_line),
                            });
                        }
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }

    /// Recursively find `call_expression` nodes inside a given node and create
    /// unresolved Calls references. A `func_literal` is not a graph symbol, so
    /// calls in its body (`go func() { work() }()`) belong to the enclosing
    /// function.
    fn extract_call_sites(state: &mut ExtractionState, node: TsNode<'_>, fn_node_id: &str) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "call_expression"
                    && let Some(callee) = child.named_child(0)
                    && callee.kind() != "func_literal"
                {
                    // The callee is an identifier or a selector_expression.
                    state.unresolved_refs.push(UnresolvedRef {
                        from_node_id: fn_node_id.to_string(),
                        reference_name: state.node_text(callee).to_string(),
                        reference_kind: EdgeKind::Calls,
                        line: child.start_position().row as u32,
                        column: child.start_position().column as u32,
                        file_path: state.file_path.clone(),
                        unmodeled_import: None,
                        argument_count: None,
                    });
                }
                Self::extract_call_sites(state, child, fn_node_id);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract the function/method signature (everything up to the body `{`).
    fn extract_signature(state: &ExtractionState, node: TsNode<'_>) -> String {
        let text = state.node_text(node);
        if let Some(brace_pos) = text.find('{') {
            text[..brace_pos].trim().to_string()
        } else {
            text.trim().to_string()
        }
    }

    /// Extract docstrings from preceding comment nodes.
    fn extract_docstring(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        docstring_from_preceding_comments(state.source, node, clean_c_comment)
    }

    /// Determine Go visibility: uppercase first character means exported (Pub),
    /// lowercase means unexported (Private).
    fn go_visibility(name: &str) -> Visibility {
        if name.starts_with(|c: char| c.is_uppercase()) {
            Visibility::Pub
        } else {
            Visibility::Private
        }
    }
}

impl crate::LanguageExtractor for GoExtractor {
    fn extensions(&self) -> &[&str] {
        &["go", "mod"]
    }

    fn language_name(&self) -> &'static str {
        "Go"
    }

    fn extract_parsed_artifact_prepared(
        &self,
        file_path: &str,
        source: &str,
        _parsed_source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        GoExtractor::extract_tree(file_path, source, tree, scope)
    }
}

/// The package name an unaliased import binds by convention: the last path
/// element, skipping a major-version element (`/v2`) and a `gopkg.in`
/// version suffix (`yaml.v3`). A package declaring another name binds that
/// name instead, which the seal then finds no import for.
fn go_default_package_name(path: &str) -> &str {
    let is_version = |segment: &str| {
        segment
            .strip_prefix('v')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut segments = path.rsplit('/');
    let last = segments.next().unwrap_or(path);
    let name = match segments.next() {
        Some(previous) if is_version(last) => previous,
        _ => last,
    };
    match name.rsplit_once('.') {
        Some((stem, version)) if is_version(version) => stem,
        _ => name,
    }
}

/// Whether `name` is one of Go's predeclared type identifiers, which name
/// the same type in every package.
fn is_predeclared_type(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "bool"
            | "byte"
            | "comparable"
            | "complex64"
            | "complex128"
            | "error"
            | "float32"
            | "float64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "rune"
            | "string"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
    )
}
