/// Tree-sitter based Rust source code extractor.
///
/// Parses Rust source files and emits nodes and edges for the code graph.
use std::{
    collections::{BTreeMap, btree_map::Entry},
    time::Instant,
};

use tree_sitter::{Node as TsNode, Tree};

use crate::common::local_node_id;
use crate::complexity::{RUST_COMPLEXITY, count_complexity};
use crate::extraction_artifact::{
    ExtractedImportEvidenceV1, ExtractionArtifactV1, ImportNamespaceV1, ImportReexportScopeV1,
    import_module_kind,
};
use crate::traversal::find_direct_child_by_kind;
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, ExtractionResult, Node, NodeKind, SourceSpan,
    UnresolvedRef, Visibility, generate_node_id,
};

/// Extracts code graph nodes and edges from Rust source files using tree-sitter.
pub struct RustExtractor;

#[derive(Default)]
struct ShadowedCallNames {
    names: Vec<String>,
}

/// Receiver bindings whose type the function body states outright: typed
/// parameters, typed `let`s, `let`s initialised by a struct literal
/// (`T { .. }`, possibly behind `?`), and `self` in a method. A dotted
/// call on such a binding also names the method by its type
/// (`builder.build()` → `ignore::WalkBuilder::build`), which is the only form
/// the resolver can bind across files. Method calls and constructor-like names
/// (`new`, `with_*`, `from_*`, `default`) are never treated as return-type evidence.
/// Rust does not require those associated functions to return their owning
/// type. Bindings are function-scoped: a name bound more than once to
/// different or unknown types is withheld rather than guessed.
#[derive(Default)]
struct ReceiverTypes {
    by_name: BTreeMap<String, Option<String>>,
}

impl ReceiverTypes {
    fn record(&mut self, name: String, type_path: Option<String>) {
        match self.by_name.entry(name) {
            Entry::Vacant(vacant) => {
                vacant.insert(type_path);
            }
            Entry::Occupied(mut occupied) => {
                if *occupied.get() != type_path {
                    occupied.insert(None);
                }
            }
        }
    }

    fn type_of(&self, name: &str) -> Option<&str> {
        self.by_name.get(name)?.as_deref()
    }
}

/// One type parameter's trait bounds, as a method owner.
///
/// `T: Processor` makes `value.process()` the callee `Processor::process`.
/// Two bounds, or a bound the syntax does not name, stay unresolved so the
/// call is not attached to both traits. This does not rename `self`: #1814
/// still records that binding from the enclosing type.
enum ParamBound {
    Unbound,
    Unique(String),
    Ambiguous,
}

#[derive(Default)]
struct TraitBounds {
    parameters: BTreeMap<String, ParamBound>,
}

struct BoundClause {
    paths: Vec<String>,
    ambiguous: bool,
}

impl TraitBounds {
    fn declare(&mut self, name: String) {
        self.parameters.insert(name, ParamBound::Unbound);
    }

    fn knows(&self, name: &str) -> bool {
        self.parameters.contains_key(name)
    }

    fn constrain(&mut self, name: &str, clause: BoundClause) {
        if clause.paths.is_empty() && !clause.ambiguous {
            return;
        }
        let Some(slot) = self.parameters.get_mut(name) else {
            return;
        };
        if clause.ambiguous || clause.paths.len() != 1 {
            *slot = ParamBound::Ambiguous;
            return;
        }
        let Some(path) = clause.paths.into_iter().next() else {
            *slot = ParamBound::Ambiguous;
            return;
        };
        match slot {
            ParamBound::Unbound => *slot = ParamBound::Unique(path),
            ParamBound::Unique(existing) if existing == &path => {}
            ParamBound::Unique(_) | ParamBound::Ambiguous => *slot = ParamBound::Ambiguous,
        }
    }

    /// Replace a written type-parameter name with its unique trait. Any other
    /// path, including the enclosing type recorded for `self`, is unchanged.
    fn resolve(&self, path: String) -> Option<String> {
        match self.parameters.get(path.as_str()) {
            Some(ParamBound::Unique(bound)) => Some(bound.clone()),
            Some(ParamBound::Ambiguous) => None,
            Some(ParamBound::Unbound) | None => Some(path),
        }
    }
}

/// Internal state used during AST traversal.
///
/// Borrows the caller's source for the lifetime of the walk: copying the
/// whole file here made every `extract_parsed` pass, including incremental
/// walks of one tiny item, pay a full-file memcpy before visiting a node.
struct ExtractionState<'s> {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_refs: Vec<UnresolvedRef>,
    errors: Vec<String>,
    imports: Vec<ExtractedImportEvidenceV1>,
    root_modules: BTreeMap<String, String>,
    /// Stack of (name, `node_id`) for building qualified names and parent edges.
    node_stack: Vec<(String, String)>,
    file_path: String,
    source: &'s [u8],
    timestamp: u64,
}

impl<'s> ExtractionState<'s> {
    fn new(file_path: &str, source: &'s str) -> Self {
        let timestamp = crate::common::unix_timestamp_secs();
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_refs: Vec::new(),
            errors: Vec::new(),
            imports: Vec::new(),
            root_modules: BTreeMap::new(),
            node_stack: Vec::new(),
            file_path: file_path.to_string(),
            source: source.as_bytes(),
            timestamp,
        }
    }

    /// Returns the current qualified name prefix from the node stack.
    ///
    /// The file root is pushed onto `node_stack` as the first frame when
    /// extraction begins, so iterating the stack already yields the file
    /// path as the leading segment.
    fn qualified_prefix(&self) -> String {
        self.node_stack
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }

    /// Returns the current parent node ID, or None if at file root level.
    fn parent_node_id(&self) -> Option<&str> {
        self.node_stack.last().map(|(_, id)| id.as_str())
    }

    /// Gets the text of a tree-sitter node, borrowed from the source.
    ///
    /// The `'s` lifetime is tied to the source, not `&self`, so callers can
    /// keep the text across mutations of the state. Signature helpers slice
    /// a small prefix from it and own only that prefix.
    fn node_text(&self, node: TsNode<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("<invalid utf8>")
    }
}

impl RustExtractor {
    fn extract_tree_artifact(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        let start = Instant::now();
        let mut state = ExtractionState::new(file_path, source);
        state.root_modules = Self::root_module_names(&state, tree.root_node());

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

        let metrics = crate::parsed_extraction::visit_root_children(tree, scope, |child| {
            Self::visit_node(&mut state, child);
        });

        state.node_stack.pop();

        crate::parsed_extraction::ParsedExtractionArtifactV1::complete(
            Self::build_artifact(state, start),
            scope,
            metrics,
        )
    }

    fn visit_children(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                Self::visit_node(state, child);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn visit_node(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        match node.kind() {
            "function_item" | "function_signature_item" => Self::visit_function(state, node),
            "struct_item" => Self::visit_struct(state, node),
            "enum_item" => Self::visit_enum(state, node),
            "trait_item" => Self::visit_trait(state, node),
            "impl_item" => Self::visit_impl(state, node),
            "use_declaration" => Self::visit_use(state, node),
            "const_item" => Self::visit_const(state, node),
            "static_item" => Self::visit_static(state, node),
            "type_item" => Self::visit_type_alias(state, node),
            "mod_item" => Self::visit_module(state, node),
            "macro_invocation" => Self::visit_macro_invocation(state, node),
            _ => {
                Self::visit_children(state, node);
            }
        }
    }

    /// Extract a function or free function node.
    fn visit_function(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let is_inside_impl = state
            .node_stack
            .iter()
            .any(|(_, id)| id.starts_with("impl:"));
        let is_inside_trait = state
            .node_stack
            .iter()
            .any(|(_, id)| id.starts_with("trait:"));
        let kind = if is_inside_impl || is_inside_trait {
            NodeKind::Method
        } else {
            NodeKind::Function
        };
        let visibility = if is_inside_trait {
            state
                .parent_node_id()
                .and_then(|parent_id| state.nodes.iter().rev().find(|node| node.id == parent_id))
                .filter(|parent| parent.kind == NodeKind::Trait)
                .map(|parent| parent.visibility.clone())
                .unwrap_or_else(|| Self::extract_visibility(node, state))
        } else {
            Self::extract_visibility(node, state)
        };
        let signature = Some(Self::extract_function_signature(state, node));
        let docstring = Self::extract_docstring(state, node);
        let is_async = Self::detect_async(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(&state.file_path, state.source, &kind, &name, node);
        let metrics = count_complexity(node, &RUST_COMPLEXITY, state.source);

        let graph_node = Node {
            id: id.clone(),
            kind,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
            end_line,
            start_column,
            end_column,
            signature,
            docstring,
            visibility,
            is_async,
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        let mut receivers = ReceiverTypes::default();
        Self::collect_receiver_types(state, node, node, &mut receivers);
        Self::extract_call_sites(state, node, &id, &receivers);
        Self::suppress_shadowed_calls(state, node, &id);

        Self::extract_annotations_from_modifiers(state, node, &id);

        // Emit TypeOf refs for parameter types and Returns refs for the return
        // type. Lets refactoring tools cluster items "anchored on T" without
        // walking source again.
        if let Some(params) = node.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if let Some(ty) = child.child_by_field_name("type") {
                        Self::emit_type_refs(state, ty, &id, EdgeKind::TypeOf);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            Self::emit_type_refs(state, ret, &id, EdgeKind::Returns);
        }
    }

    /// Extract a struct node and its fields.
    fn visit_struct(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let visibility = Self::extract_visibility(node, state);
        let signature = Some(Self::extract_struct_signature(state, node));
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Struct,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Struct,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        Self::extract_derive_macros(state, node, &id);

        Self::extract_annotations_from_modifiers(state, node, &id);

        state.node_stack.push((name, id.clone()));
        Self::extract_fields(state, node);
        state.node_stack.pop();
    }

    /// Extract an enum node and its variants.
    fn visit_enum(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let visibility = Self::extract_visibility(node, state);
        let docstring = Self::extract_docstring(state, node);
        let text = state.node_text(node);
        let signature = Some(text.lines().next().unwrap_or("").to_string());
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(&state.file_path, state.source, &NodeKind::Enum, &name, node);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Enum,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        Self::extract_derive_macros(state, node, &id);

        Self::extract_annotations_from_modifiers(state, node, &id);

        state.node_stack.push((name, id.clone()));
        Self::extract_enum_variants(state, node);
        state.node_stack.pop();
    }

    /// Extract a trait node and its methods.
    fn visit_trait(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let visibility = Self::extract_visibility(node, state);
        let docstring = Self::extract_docstring(state, node);
        let signature = Some(format!("trait {name}"));
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Trait,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Trait,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        Self::extract_annotations_from_modifiers(state, node, &id);

        // Supertrait bounds (`trait Leaf: Middle + Base`). Emit one
        // unresolved `Extends` ref per bound so the resolver can hook them
        // up to the corresponding trait nodes. Each bound is a
        // `type_identifier` reachable through the `bounds: trait_bounds`
        // field, possibly wrapped in `higher_ranked_trait_bound`. We pull
        // the right-most identifier from each bound to ignore lifetime
        // params (`'a`) and generic args.
        if let Some(bounds) = node.child_by_field_name("bounds") {
            let mut cursor = bounds.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    let kind = child.kind();
                    if kind != ","
                        && kind != ":"
                        && kind != "+"
                        && let Some(bound_name) = Self::extract_trait_bound_name(state, child)
                    {
                        state.unresolved_refs.push(UnresolvedRef {
                            from_node_id: id.clone(),
                            reference_name: bound_name,
                            reference_kind: EdgeKind::Extends,
                            line: child.start_position().row as u32,
                            column: child.start_position().column as u32,
                            file_path: state.file_path.clone(),
                        });
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        // Visit trait body: methods inside become Method nodes.
        state.node_stack.push((name, id));
        if let Some(body) = node.child_by_field_name("body") {
            Self::visit_children(state, body);
        }
        state.node_stack.pop();
    }

    /// Extract the trait identifier from a single bound node. Returns
    /// `None` for lifetime params or anything that isn't a named trait.
    fn extract_trait_bound_name(state: &ExtractionState<'_>, bound: TsNode<'_>) -> Option<String> {
        match bound.kind() {
            "type_identifier" => Some(state.node_text(bound).to_string()),
            // `Module::Trait` or `Trait<Generics>`. Take the right-most
            // identifier so we ignore module paths and generic args.
            "scoped_type_identifier" | "generic_type" => {
                let mut cursor = bound.walk();
                let mut name = None;
                if cursor.goto_first_child() {
                    loop {
                        let child = cursor.node();
                        if child.kind() == "type_identifier" {
                            name = Some(state.node_text(child).to_string());
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                name
            }
            // `for<'a> Trait` shape.
            "higher_ranked_trait_bound" => bound
                .child_by_field_name("type")
                .and_then(|inner| Self::extract_trait_bound_name(state, inner)),
            _ => None,
        }
    }

    /// Extract an impl block node and its methods.
    fn visit_impl(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let type_name =
            Self::extract_impl_type_name(state, node).unwrap_or_else(|| "<unknown>".to_string());
        let trait_name = Self::extract_impl_trait_name(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), type_name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Impl,
            &type_name,
            node,
        );

        let signature = if let Some(ref trait_n) = trait_name {
            Some(format!("impl {trait_n} for {type_name}"))
        } else {
            Some(format!("impl {type_name}"))
        };

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Impl,
            name: type_name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
            end_line,
            start_column,
            end_column,
            signature,
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        // If this is a trait impl, create an Implements edge/ref.
        if let Some(ref trait_n) = trait_name {
            state.unresolved_refs.push(UnresolvedRef {
                from_node_id: id.clone(),
                reference_name: trait_n.clone(),
                reference_kind: EdgeKind::Implements,
                line: start_line,
                column: start_column,
                file_path: state.file_path.clone(),
            });
        }

        Self::extract_annotations_from_modifiers(state, node, &id);

        // Visit impl body: functions become Method nodes.
        let method_owner = trait_name.as_ref().map_or(type_name.clone(), |trait_name| {
            format!("<{type_name} as {trait_name}>")
        });
        state.node_stack.push((method_owner, id));
        if let Some(body) = node.child_by_field_name("body") {
            Self::visit_children(state, body);
        }
        state.node_stack.pop();
    }

    /// Extract a use declaration node.
    fn visit_use(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let text = state.node_text(node);
        // Strip the `use ` prefix and trailing `;`.
        let path = text
            .trim()
            .strip_prefix("use ")
            .unwrap_or(text)
            .trim_end_matches(';')
            .trim()
            .to_string();
        let visibility = Self::extract_visibility(node, state);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let first_import = state.imports.len();
        let top_level_argument = node
            .parent()
            .filter(|parent| parent.kind() == "source_file")
            .and_then(|_| node.child_by_field_name("argument"));
        if let Some(argument) = top_level_argument {
            let (is_public, reexport_scope) = Self::use_reexport_visibility(node, state);
            Self::extract_use_bindings(state, argument, None, is_public, reexport_scope.as_ref());
        }
        let qualified_name = format!("{}::{}", state.qualified_prefix(), path);
        let id = local_node_id(&state.file_path, state.source, &NodeKind::Use, &path, node);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Use,
            name: path.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        // Named bindings cannot account for wildcard members. Retain the original
        // declaration as unresolved evidence rather than dropping those dependencies.
        if top_level_argument.is_none() || state.imports.len() == first_import || path.contains('*')
        {
            state.unresolved_refs.push(UnresolvedRef {
                from_node_id: id.clone(),
                reference_name: path,
                reference_kind: EdgeKind::Uses,
                line: start_line,
                column: start_column,
                file_path: state.file_path.clone(),
            });
        }
        if top_level_argument.is_some() {
            for import in &state.imports[first_import..] {
                if let Some(local_name) = import.local_name.as_deref() {
                    if import.module_specifier == "self"
                        && import.imported_name.as_deref() == Some(local_name)
                        && state.root_modules.contains_key(local_name)
                    {
                        continue;
                    }
                    let from_node_id = Self::use_binding_anchor(state, import)
                        .unwrap_or(id.as_str())
                        .to_owned();
                    state.unresolved_refs.push(UnresolvedRef {
                        from_node_id,
                        reference_name: local_name.to_owned(),
                        reference_kind: EdgeKind::Uses,
                        line: import.start_line,
                        column: import.start_column,
                        file_path: state.file_path.clone(),
                    });
                }
            }
        }
    }

    fn extract_use_bindings(
        state: &mut ExtractionState<'_>,
        node: TsNode<'_>,
        prefix: Option<&str>,
        is_public: bool,
        reexport_scope: Option<&ImportReexportScopeV1>,
    ) {
        match node.kind() {
            "scoped_use_list" => {
                let path = node
                    .child_by_field_name("path")
                    .map(|path| state.node_text(path));
                let combined = Self::join_use_path(prefix, path);
                if let Some(list) = node.child_by_field_name("list") {
                    Self::extract_use_bindings(
                        state,
                        list,
                        combined.as_deref(),
                        is_public,
                        reexport_scope,
                    );
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    Self::extract_use_bindings(state, child, prefix, is_public, reexport_scope);
                }
            }
            "use_as_clause" => {
                let Some(path) = node.child_by_field_name("path") else {
                    return;
                };
                let Some(alias) = node.child_by_field_name("alias") else {
                    return;
                };
                let full_path = Self::join_use_path(prefix, Some(state.node_text(path)));
                if let Some(full_path) = full_path {
                    Self::push_use_binding(
                        state,
                        &full_path,
                        state.node_text(alias),
                        node,
                        is_public,
                        reexport_scope,
                    );
                }
            }
            "use_wildcard" => {
                let text = state.node_text(node);
                let module = node
                    .child_by_field_name("path")
                    .map(|path| state.node_text(path))
                    .or(prefix)
                    .or_else(|| text.strip_suffix("::*"));
                if let Some(module) = module {
                    Self::push_glob_binding(state, module, node, is_public, reexport_scope);
                }
            }
            _ => {
                let full_path = Self::join_use_path(prefix, Some(state.node_text(node)));
                if let Some(full_path) = full_path {
                    let local_name = full_path.rsplit("::").next().unwrap_or(full_path.as_str());
                    Self::push_use_binding(
                        state,
                        &full_path,
                        local_name,
                        node,
                        is_public,
                        reexport_scope,
                    );
                }
            }
        }
    }

    fn join_use_path(prefix: Option<&str>, path: Option<&str>) -> Option<String> {
        match (prefix, path) {
            (Some(prefix), Some("self")) => Some(prefix.to_owned()),
            (Some(prefix), Some(path)) => Some(format!("{prefix}::{path}")),
            (Some(prefix), None) => Some(prefix.to_owned()),
            (None, Some(path)) if !path.is_empty() => Some(path.to_owned()),
            (None, _) => None,
        }
    }

    fn push_use_binding(
        state: &mut ExtractionState<'_>,
        full_path: &str,
        local_name: &str,
        evidence_node: TsNode<'_>,
        is_public: bool,
        reexport_scope: Option<&ImportReexportScopeV1>,
    ) {
        let (module_specifier, imported_name) = match full_path.rsplit_once("::") {
            Some(parts) => parts,
            None if Self::declares_module(state, full_path) => ("self", full_path),
            None => return,
        };
        let module_specifier = Self::canonical_rust_import_module(state, module_specifier);
        let Some(module_kind) = import_module_kind("rust", &module_specifier) else {
            return;
        };
        state.imports.push(ExtractedImportEvidenceV1 {
            logical_path: state.file_path.clone(),
            module_specifier,
            imported_name: Some(imported_name.to_owned()),
            local_name: Some(local_name.to_owned()),
            is_public,
            reexport_scope: reexport_scope.cloned(),
            is_glob: false,
            namespace: ImportNamespaceV1::Value,
            module_kind,
            span: SourceSpan {
                start_byte: evidence_node.start_byte() as u64,
                end_byte: evidence_node.end_byte() as u64,
            },
            start_line: evidence_node.start_position().row as u32,
            start_column: evidence_node.start_position().column as u32,
        });
    }

    fn push_glob_binding(
        state: &mut ExtractionState<'_>,
        module: &str,
        evidence_node: TsNode<'_>,
        is_public: bool,
        reexport_scope: Option<&ImportReexportScopeV1>,
    ) {
        let module_specifier = Self::canonical_rust_import_module(state, module);
        let Some(module_kind) = import_module_kind("rust", &module_specifier) else {
            return;
        };
        state.imports.push(ExtractedImportEvidenceV1 {
            logical_path: state.file_path.clone(),
            module_specifier,
            imported_name: Some("*".to_owned()),
            local_name: None,
            is_public,
            reexport_scope: reexport_scope.cloned(),
            is_glob: true,
            namespace: ImportNamespaceV1::Value,
            module_kind,
            span: SourceSpan {
                start_byte: evidence_node.start_byte() as u64,
                end_byte: evidence_node.end_byte() as u64,
            },
            start_line: evidence_node.start_position().row as u32,
            start_column: evidence_node.start_position().column as u32,
        });
    }

    fn canonical_rust_import_module(state: &ExtractionState<'_>, module: &str) -> String {
        let first = module.split("::").next().unwrap_or(module);
        if matches!(first, "crate" | "self" | "super") || !Self::declares_module(state, first) {
            module.to_owned()
        } else {
            format!("self::{module}")
        }
    }

    fn declares_module(state: &ExtractionState<'_>, name: &str) -> bool {
        state.root_modules.contains_key(name)
    }

    fn use_binding_anchor<'a>(
        state: &'a ExtractionState<'_>,
        import: &ExtractedImportEvidenceV1,
    ) -> Option<&'a str> {
        let module = import
            .module_specifier
            .strip_prefix("self::")
            .and_then(|path| path.split("::").next())
            .or_else(|| {
                (import.module_specifier == "self")
                    .then_some(import.imported_name.as_deref())
                    .flatten()
            })?;
        state.root_modules.get(module).map(String::as_str)
    }

    fn root_module_names(
        state: &ExtractionState<'_>,
        root: TsNode<'_>,
    ) -> BTreeMap<String, String> {
        let mut cursor = root.walk();
        root.named_children(&mut cursor)
            .filter(|child| child.kind() == "mod_item")
            .filter_map(|child| {
                let name = child.child_by_field_name("name")?;
                let name = state.node_text(name).to_owned();
                let id = local_node_id(
                    &state.file_path,
                    state.source,
                    &NodeKind::Module,
                    &name,
                    child,
                );
                Some((name, id))
            })
            .collect()
    }

    /// Extract a const item node.
    fn visit_const(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let visibility = Self::extract_visibility(node, state);
        let docstring = Self::extract_docstring(state, node);
        let text = state.node_text(node);
        let signature = Some(text.lines().next().unwrap_or("").to_string());
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
            attrs_start_line: Self::compute_attrs_start_line(node),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract a static item node.
    fn visit_static(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let visibility = Self::extract_visibility(node, state);
        let docstring = Self::extract_docstring(state, node);
        let text = state.node_text(node);
        let signature = Some(text.lines().next().unwrap_or("").to_string());
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
            attrs_start_line: Self::compute_attrs_start_line(node),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract a type alias node.
    fn visit_type_alias(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let visibility = Self::extract_visibility(node, state);
        let docstring = Self::extract_docstring(state, node);
        let text = state.node_text(node);
        let signature = Some(text.trim().to_string());
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::TypeAlias,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::TypeAlias,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract a module item node.
    fn visit_module(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let visibility = Self::extract_visibility(node, state);
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Module,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Module,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
            end_line,
            start_column,
            end_column,
            signature: Some(format!("mod {name}")),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        Self::extract_annotations_from_modifiers(state, node, &id);

        state.node_stack.push((name, id));
        if let Some(body) = node.child_by_field_name("body") {
            Self::visit_children(state, body);
        }
        state.node_stack.pop();
    }

    /// Record a macro invocation as an unresolved call reference.
    fn visit_macro_invocation(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let macro_name = node.child_by_field_name("macro").map_or_else(
            || {
                // Fallback: first named child is typically the macro name.
                let text = state.node_text(node);
                text.split('!').next().unwrap_or("").trim().to_string()
            },
            |n| state.node_text(n).to_string(),
        );
        let start_line = node.start_position().row as u32;
        let start_column = node.start_position().column as u32;

        if let Some(parent_id) = state.parent_node_id() {
            state.unresolved_refs.push(UnresolvedRef {
                from_node_id: parent_id.to_string(),
                reference_name: macro_name,
                reference_kind: EdgeKind::Calls,
                line: start_line,
                column: start_column,
                file_path: state.file_path.clone(),
            });
        }
    }

    /// Extract the name of a node by looking for a "name" field child.
    fn extract_name(state: &ExtractionState<'_>, node: TsNode<'_>) -> Option<String> {
        node.child_by_field_name("name")
            .map(|n| state.node_text(n).to_string())
    }

    /// Extract the type name from an `impl_item` (the "type" field).
    fn extract_impl_type_name(state: &ExtractionState<'_>, node: TsNode<'_>) -> Option<String> {
        node.child_by_field_name("type")
            .map(|n| state.node_text(n).to_string())
    }

    /// Extract the trait name from an `impl_item`, if it is a trait impl.
    ///
    /// For `impl Trait for Type`, tree-sitter gives us a "trait" field.
    fn extract_impl_trait_name(state: &ExtractionState<'_>, node: TsNode<'_>) -> Option<String> {
        node.child_by_field_name("trait")
            .map(|n| state.node_text(n).to_string())
    }

    fn use_reexport_visibility(
        node: TsNode<'_>,
        state: &ExtractionState<'_>,
    ) -> (bool, Option<ImportReexportScopeV1>) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return (false, None);
        }
        loop {
            let child = cursor.node();
            if child.kind() == "visibility_modifier" {
                let visibility = state.node_text(child);
                return match visibility {
                    "pub" => (true, None),
                    "pub(crate)" | "pub(in crate)" => (false, Some(ImportReexportScopeV1::Crate)),
                    "pub(super)" | "pub(in super)" => (false, Some(ImportReexportScopeV1::Super)),
                    "pub(self)" | "pub(in self)" => {
                        (false, Some(ImportReexportScopeV1::SelfModule))
                    }
                    _ => (
                        false,
                        visibility
                            .strip_prefix("pub(in crate::")
                            .and_then(|module| module.strip_suffix(')'))
                            .filter(|module| !module.is_empty())
                            .map(|module| ImportReexportScopeV1::Module(module.replace("::", "/"))),
                    ),
                };
            }
            if !cursor.goto_next_sibling() {
                return (false, None);
            }
        }
    }

    /// Extract visibility from a node.
    fn extract_visibility(node: TsNode<'_>, state: &ExtractionState<'_>) -> Visibility {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "visibility_modifier" {
                    let text = state.node_text(child);
                    return match text {
                        "pub" => Visibility::Pub,
                        s if s.contains("crate") => Visibility::PubCrate,
                        s if s.contains("super") => Visibility::PubSuper,
                        _ => Visibility::Pub,
                    };
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        Visibility::Private
    }

    /// Extract the function signature (everything from `fn` up to the body `{`).
    fn extract_function_signature(state: &ExtractionState<'_>, node: TsNode<'_>) -> String {
        let text = state.node_text(node);
        if let Some(body) = node.child_by_field_name("body") {
            let offset = body.start_byte() - node.start_byte();
            text[..offset].trim().to_string()
        } else if let Some(brace_pos) = text.find('{') {
            text[..brace_pos].trim().to_string()
        } else {
            // For trait method declarations without a body (ending with `;`).
            text.trim_end_matches(';').trim().to_string()
        }
    }

    /// Extract the struct signature (the header line).
    fn extract_struct_signature(state: &ExtractionState<'_>, node: TsNode<'_>) -> String {
        let text = state.node_text(node);
        if let Some(body) = node.child_by_field_name("body") {
            let offset = body.start_byte() - node.start_byte();
            text[..offset].trim().to_string()
        } else if let Some(brace_pos) = text.find('{') {
            text[..brace_pos].trim().to_string()
        } else {
            text.lines().next().unwrap_or("").trim().to_string()
        }
    }

    /// Extract docstrings from preceding comment nodes.
    fn extract_docstring(state: &ExtractionState<'_>, node: TsNode<'_>) -> Option<String> {
        let mut comments = Vec::new();
        let mut current = node.prev_named_sibling();
        while let Some(sibling) = current {
            match sibling.kind() {
                "line_comment" | "block_comment" => {
                    let text = state.node_text(sibling);
                    comments.push(text);
                    current = sibling.prev_named_sibling();
                }
                "attribute_item" => {
                    // Skip attributes (like #[derive(...)]) that sit between doc
                    // comments and the item.
                    current = sibling.prev_named_sibling();
                }
                _ => break,
            }
        }
        if comments.is_empty() {
            return None;
        }
        // Comments are collected in reverse order (closest first).
        comments.reverse();
        let cleaned: Vec<String> = comments.iter().map(|c| Self::clean_comment(c)).collect();
        let result = cleaned.join("\n").trim().to_string();
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    /// Strip comment markers from a single comment text.
    fn clean_comment(comment: &str) -> String {
        let trimmed = comment.trim();
        if let Some(stripped) = trimmed.strip_prefix("///") {
            stripped.strip_prefix(' ').unwrap_or(stripped).to_string()
        } else if let Some(stripped) = trimmed.strip_prefix("//!") {
            stripped.strip_prefix(' ').unwrap_or(stripped).to_string()
        } else if let Some(stripped) = trimmed.strip_prefix("//") {
            stripped.strip_prefix(' ').unwrap_or(stripped).to_string()
        } else if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
            let inner = &trimmed[2..trimmed.len() - 2];
            inner
                .lines()
                .map(|line| {
                    let l = line.trim();
                    l.strip_prefix("* ")
                        .or_else(|| l.strip_prefix('*'))
                        .unwrap_or(l)
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// Detect if a function is async.
    fn detect_async(state: &ExtractionState<'_>, node: TsNode<'_>) -> bool {
        let text = state.node_text(node);
        let trimmed = text.trim_start();
        trimmed.starts_with("async ")
            || trimmed.starts_with("pub async ")
            || trimmed.starts_with("pub(crate) async ")
            || trimmed.starts_with("pub(super) async ")
    }

    /// Extract fields from a struct's `field_declaration_list`.
    fn extract_fields(state: &mut ExtractionState<'_>, struct_node: TsNode<'_>) {
        if let Some(body) = struct_node.child_by_field_name("body") {
            let mut cursor = body.walk();
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

    /// Extract a single `field_declaration` node.
    fn extract_single_field(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let visibility = Self::extract_visibility(node, state);
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
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().trim_end_matches(',').trim().to_string()),
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        // Emit TypeOf refs for the field's declared type.
        if let Some(type_node) = node.child_by_field_name("type") {
            Self::emit_type_refs(state, type_node, &id, EdgeKind::TypeOf);
        }
    }

    /// Extract enum variants from the enum body.
    fn extract_enum_variants(state: &mut ExtractionState<'_>, enum_node: TsNode<'_>) {
        if let Some(body) = enum_node.child_by_field_name("body") {
            let mut cursor = body.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "enum_variant" {
                        Self::extract_single_variant(state, child);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }

    /// Extract a single enum variant.
    fn extract_single_variant(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::extract_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let text = state.node_text(node);
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::EnumVariant,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::EnumVariant,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().trim_end_matches(',').to_string()),
            docstring,
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

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Recursively find `call_expression` nodes inside a given node and create
    /// unresolved Calls references.
    fn extract_call_sites(
        state: &mut ExtractionState<'_>,
        node: TsNode<'_>,
        fn_node_id: &str,
        receivers: &ReceiverTypes,
    ) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "call_expression" => {
                        if let Some(callee) = child.child_by_field_name("function") {
                            let member_callee = if callee.kind() == "generic_function" {
                                callee.child_by_field_name("function").unwrap_or(callee)
                            } else {
                                callee
                            };
                            let receiver = (member_callee.kind() == "field_expression")
                                .then(|| {
                                    member_callee
                                        .child_by_field_name("value")
                                        .zip(member_callee.child_by_field_name("field"))
                                })
                                .flatten()
                                .filter(|(_, field)| field.kind() == "field_identifier");
                            // Receiver spelling is not target authority. The field
                            // node owns the member identity and exact token site,
                            // independently of comments or whitespace after `.`.
                            let (callee_name, position) = receiver.map_or_else(
                                || (state.node_text(callee).to_owned(), child.start_position()),
                                |(value, field)| {
                                    (
                                        format!(
                                            "{}.{}",
                                            state.node_text(value),
                                            state.node_text(field)
                                        ),
                                        field.start_position(),
                                    )
                                },
                            );
                            state.unresolved_refs.push(UnresolvedRef {
                                from_node_id: fn_node_id.to_string(),
                                reference_name: callee_name,
                                reference_kind: EdgeKind::Calls,
                                line: position.row as u32,
                                column: position.column as u32,
                                file_path: state.file_path.clone(),
                            });
                            // The simple name of a dotted call is not itself a call.
                            // `items.push()` must not bind a same-file `fn push`.
                            // Only a stated receiver type names the method
                            // (`Rows::len`), which is also the form that binds
                            // across files.
                            if let Some(typed_method) =
                                Self::typed_receiver_method(state, member_callee, receivers)
                            {
                                state.unresolved_refs.push(UnresolvedRef {
                                    from_node_id: fn_node_id.to_string(),
                                    reference_name: typed_method,
                                    reference_kind: EdgeKind::Calls,
                                    line: position.row as u32,
                                    column: position.column as u32,
                                    file_path: state.file_path.clone(),
                                });
                            }
                        }
                        Self::extract_call_sites(state, child, fn_node_id, receivers);
                    }
                    "macro_invocation" => {
                        let macro_name = child.child_by_field_name("macro").map_or_else(
                            || {
                                let text = state.node_text(child);
                                text.split('!').next().unwrap_or("").trim().to_string()
                            },
                            |n| state.node_text(n).to_string(),
                        );
                        state.unresolved_refs.push(UnresolvedRef {
                            from_node_id: fn_node_id.to_string(),
                            reference_name: macro_name,
                            reference_kind: EdgeKind::Calls,
                            line: child.start_position().row as u32,
                            column: child.start_position().column as u32,
                            file_path: state.file_path.clone(),
                        });
                        Self::extract_call_sites(state, child, fn_node_id, receivers);
                    }
                    // Inside a macro's token_tree, the grammar does not produce
                    // call_expression nodes. Instead, a function call appears as
                    // an identifier immediately followed by a token_tree sibling
                    // (e.g. `check_count(5)` → identifier "check_count" + token_tree
                    // "(5)"). Detect that pattern and emit Calls edges, then recurse
                    // into the token_tree to handle further nesting.
                    "token_tree" => {
                        Self::extract_calls_in_token_tree(state, child, fn_node_id);
                    }
                    // Skip nested function definitions. They are handled separately.
                    "function_item" => {}
                    _ => {
                        Self::extract_call_sites(state, child, fn_node_id, receivers);
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// `Type::method` for a `binding.method(..)` callee whose binding has one
    /// stated type in this function; `None` for every other callee shape.
    fn typed_receiver_method(
        state: &ExtractionState<'_>,
        callee: TsNode<'_>,
        receivers: &ReceiverTypes,
    ) -> Option<String> {
        if callee.kind() != "field_expression" {
            return None;
        }
        let value = callee.child_by_field_name("value")?;
        let field = callee.child_by_field_name("field")?;
        if field.kind() != "field_identifier" {
            return None;
        }
        // `self` is its own token, not an identifier. Both name a binding.
        let receiver_name = match value.kind() {
            "identifier" | "self" => state.node_text(value),
            _ => return None,
        };
        let type_path = receivers.type_of(receiver_name)?;
        Some(format!("{type_path}::{}", state.node_text(field)))
    }

    /// The type `self` names in the enclosing impl or trait, carrying the
    /// enclosing module path.
    ///
    /// Trait impls store `<Type as Trait>` so the method keeps a UFCS name.
    /// `self` still names `Type`, the path a call site writes and the alias
    /// same-file resolution binds. Same-file resolution keys a definition by
    /// its file-relative qualified name, so an impl inside `mod inner` has to
    /// name `inner::Type::method` or the call binds nothing.
    fn enclosing_receiver_type(state: &ExtractionState<'_>) -> Option<String> {
        let owner = state
            .node_stack
            .iter()
            .rposition(|(_, id)| id.starts_with("impl:") || id.starts_with("trait:"))?;
        let (name, id) = &state.node_stack[owner];
        let type_name = if id.starts_with("impl:") {
            Self::impl_owner_type_name(name)
        } else {
            name.as_str()
        };
        if type_name.is_empty()
            || type_name == "Self"
            || type_name == "<unknown>"
            || type_name == "<anonymous>"
        {
            return None;
        }
        // Frame 0 is the file root, which the qualified name drops.
        let mut path = state
            .node_stack
            .get(1..owner)
            .unwrap_or_default()
            .iter()
            .map(|(segment, _)| segment.as_str())
            .collect::<Vec<_>>();
        path.push(type_name);
        Some(path.join("::"))
    }

    /// The self type inside a stored impl owner name.
    ///
    /// A trait impl stores `<Type as Trait>`, and `Type` can itself be a
    /// projection (`<Foo as Assoc>::Item`), so the delimiter is the ` as ` at
    /// depth zero inside the wrapper, not the first one in the string.
    fn impl_owner_type_name(owner: &str) -> &str {
        let Some(inner) = owner.strip_prefix('<') else {
            return owner;
        };
        let mut depth = 0_i32;
        for (index, character) in inner.char_indices() {
            match character {
                '<' => depth += 1,
                '>' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                _ => {
                    if depth == 0 && inner[index..].starts_with(" as ") {
                        return inner[..index].trim();
                    }
                }
            }
        }
        owner
    }

    /// Records every binding the function introduces with the type it states,
    /// or `None` for a binding whose type the syntax does not state (pattern
    /// destructuring, `if let`, `match` arms, closure parameters, `for`).
    fn collect_receiver_types(
        state: &ExtractionState<'_>,
        node: TsNode<'_>,
        function: TsNode<'_>,
        receivers: &mut ReceiverTypes,
    ) {
        let bounds = Self::trait_bounds_for(state, function);
        Self::collect_receiver_bindings(state, node, function, receivers, &bounds);
    }

    fn collect_receiver_bindings(
        state: &ExtractionState<'_>,
        node: TsNode<'_>,
        function: TsNode<'_>,
        receivers: &mut ReceiverTypes,
        bounds: &TraitBounds,
    ) {
        match node.kind() {
            "self_parameter" => {
                if let Some(type_path) = Self::enclosing_receiver_type(state) {
                    receivers.record("self".to_owned(), Some(type_path));
                }
            }
            "parameter" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    let type_path = node
                        .child_by_field_name("type")
                        .and_then(|ty| Self::receiver_type_path(state, ty, bounds));
                    Self::record_receiver_pattern(state, pattern, type_path, receivers);
                }
            }
            "let_declaration" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    let type_path = match node.child_by_field_name("type") {
                        Some(ty) => Self::receiver_type_path(state, ty, bounds),
                        None => node
                            .child_by_field_name("value")
                            .and_then(|value| Self::stated_initializer_type_path(state, value)),
                    };
                    Self::record_receiver_pattern(state, pattern, type_path, receivers);
                }
            }
            "let_condition" | "match_arm" | "for_expression" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    Self::record_receiver_pattern(state, pattern, None, receivers);
                }
            }
            // Typed closure parameters are `parameter` nodes handled above;
            // untyped ones are bare patterns that shadow with unknown type.
            "closure_parameters" => {
                let mut cursor = node.walk();
                if cursor.goto_first_child() {
                    loop {
                        let child = cursor.node();
                        if child.is_named() && child.kind() != "parameter" {
                            Self::record_receiver_pattern(state, child, None, receivers);
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
        if node != function && node.kind() == "function_item" {
            return;
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                Self::collect_receiver_bindings(state, cursor.node(), function, receivers, bounds);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// A bare identifier or `self` pattern takes `type_path`; every identifier
    /// inside any other pattern is bound with an unknown type.
    fn record_receiver_pattern(
        state: &ExtractionState<'_>,
        pattern: TsNode<'_>,
        type_path: Option<String>,
        receivers: &mut ReceiverTypes,
    ) {
        if pattern.kind() == "identifier" || pattern.kind() == "self" {
            receivers.record(state.node_text(pattern).to_owned(), type_path);
            return;
        }
        let mut cursor = pattern.walk();
        if cursor.goto_first_child() {
            loop {
                Self::record_receiver_pattern(state, cursor.node(), None, receivers);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// The nominal type path a type annotation names, seen through references,
    /// generic arguments, and `dyn`/`impl` trait objects; `None` for tuples,
    /// slices, function pointers, and anything else without one nominal head.
    /// `Self` is the enclosing impl or trait type when one is on the stack.
    fn stated_type_path(state: &ExtractionState<'_>, ty: TsNode<'_>) -> Option<String> {
        match ty.kind() {
            "type_identifier" | "scoped_type_identifier" => {
                let path = state.node_text(ty);
                if path == "Self" {
                    // `Self` in an annotation is the enclosing impl or trait,
                    // not a type the file declared under that name.
                    Self::enclosing_receiver_type(state)
                } else {
                    Some(path.to_owned())
                }
            }
            "reference_type" | "generic_type" => ty
                .child_by_field_name("type")
                .and_then(|inner| Self::stated_type_path(state, inner)),
            "dynamic_type" | "abstract_type" => ty
                .child_by_field_name("trait")
                .and_then(|inner| Self::stated_type_path(state, inner)),
            "higher_ranked_trait_bound" => ty
                .child_by_field_name("type")
                .and_then(|inner| Self::stated_type_path(state, inner)),
            // `impl Trait + 'a` still names that trait. Two nominals do not.
            "bounded_type" => Self::unique_sum_type_path(state, ty),
            _ => None,
        }
    }

    /// A parameter type, with a type parameter replaced by its unique trait
    /// bound. `Self` is left as #1814 mapped it: the enclosing type, not the
    /// trait the parameter happens to implement.
    fn receiver_type_path(
        state: &ExtractionState<'_>,
        ty: TsNode<'_>,
        bounds: &TraitBounds,
    ) -> Option<String> {
        if Self::annotation_is_self(state, ty) {
            return Self::enclosing_receiver_type(state);
        }
        bounds.resolve(Self::stated_type_path(state, ty)?)
    }

    fn annotation_is_self(state: &ExtractionState<'_>, ty: TsNode<'_>) -> bool {
        match ty.kind() {
            "type_identifier" => state.node_text(ty) == "Self",
            "reference_type" => ty
                .child_by_field_name("type")
                .is_some_and(|inner| Self::annotation_is_self(state, inner)),
            _ => false,
        }
    }

    fn unique_sum_type_path(state: &ExtractionState<'_>, ty: TsNode<'_>) -> Option<String> {
        let mut found = None;
        let mut cursor = ty.walk();
        if !cursor.goto_first_child() {
            return None;
        }
        loop {
            let child = cursor.node();
            if child.is_named() {
                let path = match child.kind() {
                    "lifetime" | "use_bounds" => None,
                    "bounded_type" => Self::unique_sum_type_path(state, child),
                    _ => Self::stated_type_path(state, child),
                };
                match path {
                    None if matches!(child.kind(), "lifetime" | "use_bounds") => {}
                    None => return None,
                    Some(path) => {
                        if found.replace(path).is_some() {
                            return None;
                        }
                    }
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        found
    }

    fn trait_bounds_for(state: &ExtractionState<'_>, function: TsNode<'_>) -> TraitBounds {
        let mut ancestors = Vec::new();
        let mut current = function.parent();
        while let Some(node) = current {
            if matches!(node.kind(), "function_item" | "function_signature_item") {
                break;
            }
            if matches!(node.kind(), "impl_item" | "trait_item") {
                ancestors.push(node);
            }
            current = node.parent();
        }
        ancestors.reverse();
        let mut bounds = TraitBounds::default();
        for item in ancestors {
            Self::absorb_generic_bounds(state, item, &mut bounds);
        }
        Self::absorb_generic_bounds(state, function, &mut bounds);
        bounds
    }

    fn absorb_generic_bounds(
        state: &ExtractionState<'_>,
        item: TsNode<'_>,
        bounds: &mut TraitBounds,
    ) {
        if let Some(parameters) = item.child_by_field_name("type_parameters") {
            let mut cursor = parameters.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "type_parameter"
                        && let Some(name_node) = child.child_by_field_name("name")
                    {
                        let name = state.node_text(name_node).to_owned();
                        bounds.declare(name.clone());
                        if let Some(clause) = child.child_by_field_name("bounds") {
                            bounds.constrain(&name, Self::trait_bound_clause(state, clause));
                        }
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        let Some(where_clause) = find_direct_child_by_kind(item, "where_clause") else {
            return;
        };
        let mut cursor = where_clause.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let child = cursor.node();
            if child.kind() == "where_predicate"
                && let Some(left) = child.child_by_field_name("left")
                && left.kind() == "type_identifier"
            {
                let name = state.node_text(left);
                if bounds.knows(name)
                    && let Some(clause) = child.child_by_field_name("bounds")
                {
                    bounds.constrain(name, Self::trait_bound_clause(state, clause));
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    fn trait_bound_clause(state: &ExtractionState<'_>, bounds: TsNode<'_>) -> BoundClause {
        let mut clause = BoundClause {
            paths: Vec::new(),
            ambiguous: false,
        };
        let mut cursor = bounds.walk();
        if !cursor.goto_first_child() {
            return clause;
        }
        loop {
            let child = cursor.node();
            if child.is_named() {
                match child.kind() {
                    "lifetime" | "use_bounds" | "removed_trait_bound" => {}
                    _ => match Self::stated_type_path(state, child) {
                        Some(path) => clause.paths.push(path),
                        None => clause.ambiguous = true,
                    },
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        clause
    }

    /// The type a `let` initialiser states in syntax: a `T { .. }` literal,
    /// optionally behind `?`. Method names are never return-type evidence.
    /// Abstain rather than fabricate a receiver type.
    fn stated_initializer_type_path(
        state: &ExtractionState<'_>,
        value: TsNode<'_>,
    ) -> Option<String> {
        match value.kind() {
            "try_expression" => value
                .child(0)
                .and_then(|inner| Self::stated_initializer_type_path(state, inner)),
            "struct_expression" => value
                .child_by_field_name("name")
                .and_then(|name| Self::stated_type_path(state, name)),
            _ => None,
        }
    }

    /// Import rows are file-scoped, so a local binding makes the same bare
    /// call name ambiguous for its whole owning function. Withhold that call
    /// rather than claiming statement-level resolution the artifact lacks.
    fn suppress_shadowed_calls(
        state: &mut ExtractionState<'_>,
        function: TsNode<'_>,
        fn_node_id: &str,
    ) {
        let mut shadows = ShadowedCallNames::default();
        Self::collect_shadowed_names(state, function, function, &mut shadows);
        state.unresolved_refs.retain(|reference| {
            reference.from_node_id != fn_node_id
                || reference.reference_kind != EdgeKind::Calls
                || !shadows.names.contains(&reference.reference_name)
        });
    }

    fn collect_shadowed_names(
        state: &ExtractionState<'_>,
        node: TsNode<'_>,
        function: TsNode<'_>,
        shadows: &mut ShadowedCallNames,
    ) {
        if matches!(
            node.kind(),
            "parameter" | "let_declaration" | "for_expression"
        ) && let Some(pattern) = node.child_by_field_name("pattern")
        {
            Self::record_binding_pattern(state, pattern, shadows);
        }
        if node.kind() == "closure_expression"
            && let Some(parameters) = node.child_by_field_name("parameters")
        {
            Self::record_binding_pattern(state, parameters, shadows);
        }
        if node != function && node.kind() == "function_item" {
            return;
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                Self::collect_shadowed_names(state, cursor.node(), function, shadows);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn record_binding_pattern(
        state: &ExtractionState<'_>,
        pattern: TsNode<'_>,
        shadows: &mut ShadowedCallNames,
    ) {
        if pattern.kind() == "identifier" {
            shadows.names.push(state.node_text(pattern).to_owned());
            return;
        }
        // Only walk the parser's binding field. Destructuring property and
        // default-value subtrees may add names, deliberately withholding an
        // ambiguous edge rather than inventing one.
        let mut cursor = pattern.walk();
        if cursor.goto_first_child() {
            loop {
                Self::record_binding_pattern(state, cursor.node(), shadows);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Scan the children of a `token_tree` node (macro argument list) for
    /// function-call patterns: an `identifier` immediately followed by a
    /// `token_tree` sibling is treated as a call.  We also recurse into nested
    /// `token_tree` nodes so that deeply-nested calls are found too.
    fn extract_calls_in_token_tree(
        state: &mut ExtractionState<'_>,
        node: TsNode<'_>,
        fn_node_id: &str,
    ) {
        // Collect the named children so we can look ahead by one position.
        let mut children = Vec::new();
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                children.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }

        let mut i = 0;
        while i < children.len() {
            let cur = children[i];
            if cur.kind() == "identifier" {
                // Check whether the next sibling is a token_tree (call arguments).
                if i + 1 < children.len() && children[i + 1].kind() == "token_tree" {
                    let callee_name = state.node_text(cur);
                    state.unresolved_refs.push(UnresolvedRef {
                        from_node_id: fn_node_id.to_string(),
                        reference_name: callee_name.to_string(),
                        reference_kind: EdgeKind::Calls,
                        line: cur.start_position().row as u32,
                        column: cur.start_position().column as u32,
                        file_path: state.file_path.clone(),
                    });
                    Self::extract_calls_in_token_tree(state, children[i + 1], fn_node_id);
                    i += 2; // skip the token_tree we just handled
                    continue;
                }
            } else if cur.kind() == "token_tree" {
                // Standalone token_tree (e.g. `{…}` or `(…)` block). Recurse.
                Self::extract_calls_in_token_tree(state, cur, fn_node_id);
            } else if cur.kind() == "macro_invocation" {
                // Nested macro inside a macro. Handled via extract_call_sites.
                // Receiver types are not tracked through macro token trees.
                Self::extract_call_sites(state, cur, fn_node_id, &ReceiverTypes::default());
            }
            i += 1;
        }
    }

    /// Extract derive macros from attribute items preceding a struct/enum.
    fn extract_derive_macros(state: &mut ExtractionState<'_>, node: TsNode<'_>, item_id: &str) {
        let mut current = node.prev_named_sibling();
        while let Some(sibling) = current {
            if sibling.kind() == "attribute_item" {
                let text = state.node_text(sibling);
                if text.contains("derive") {
                    Self::parse_derive_list(state, text, item_id, sibling);
                }
                current = sibling.prev_named_sibling();
            } else if sibling.kind() == "line_comment" || sibling.kind() == "block_comment" {
                // Skip comments between attributes and the item.
                current = sibling.prev_named_sibling();
            } else {
                break;
            }
        }
    }

    /// Parse a derive attribute list and emit `DerivesMacro` edges.
    fn parse_derive_list(
        state: &mut ExtractionState<'_>,
        attr_text: &str,
        item_id: &str,
        attr_node: TsNode<'_>,
    ) {
        // attr_text is like: `#[derive(Debug, Clone, Serialize)]`
        // Find the content inside derive(...).
        if let Some(start) = attr_text.find("derive(") {
            let after = &attr_text[start + 7..];
            if let Some(end) = after.find(')') {
                let inner = &after[..end];
                let line = attr_node.start_position().row as u32;
                for trait_name in inner.split(',') {
                    let trait_name = trait_name.trim();
                    if !trait_name.is_empty() {
                        state.unresolved_refs.push(UnresolvedRef {
                            from_node_id: item_id.to_string(),
                            reference_name: trait_name.to_string(),
                            reference_kind: EdgeKind::DerivesMacro,
                            line,
                            column: attr_node.start_position().column as u32,
                            file_path: state.file_path.clone(),
                        });
                    }
                }
            }
        }
    }

    /// Returns the line of the earliest preceding doc-comment / attribute
    /// sibling of `node`, or `node`'s own start line when there is no leading
    /// block. Walks back over `attribute_item`, `line_comment`, and
    /// `block_comment` siblings; stops at the first node of any other kind.
    ///
    /// Lets refactoring tools select the full span of an item (delete, move,
    /// rewrite) without losing its leading documentation or attributes.
    fn compute_attrs_start_line(node: TsNode<'_>) -> u32 {
        let mut earliest = node.start_position().row as u32;
        let mut current = node.prev_named_sibling();
        while let Some(sibling) = current {
            match sibling.kind() {
                "attribute_item" | "line_comment" | "block_comment" => {
                    earliest = sibling.start_position().row as u32;
                    current = sibling.prev_named_sibling();
                }
                _ => break,
            }
        }
        earliest
    }

    /// Walks a type expression and emits an `UnresolvedRef` of the given kind
    /// for every named type identifier it contains. For a type like
    /// `Result<Vec<T>, MyError>` this yields refs for `Result`, `Vec`, `T`,
    /// and `MyError`, letting the resolver wire them up to declared nodes.
    fn emit_type_refs(
        state: &mut ExtractionState<'_>,
        type_node: TsNode<'_>,
        from_id: &str,
        kind: EdgeKind,
    ) {
        let mut cursor = type_node.walk();
        Self::emit_type_refs_walk(state, &mut cursor, from_id, kind);
    }

    fn emit_type_refs_walk(
        state: &mut ExtractionState<'_>,
        cursor: &mut tree_sitter::TreeCursor<'_>,
        from_id: &str,
        kind: EdgeKind,
    ) {
        let n = cursor.node();
        // A scoped type carries its own namespace (`fmt::Result`). Emitting it
        // whole, and not descending into its `path`/`name` children, is what
        // keeps resolution honest: the bare `Result` child would bind a
        // same-named local type through the simple-name index, inventing an
        // edge to a type the source never named. Resolution narrows a
        // qualified name to its simple form itself when it looks cross-file,
        // so nothing is lost by naming the reference exactly.
        if n.kind() == "scoped_type_identifier" {
            state.unresolved_refs.push(UnresolvedRef {
                from_node_id: from_id.to_string(),
                reference_name: state.node_text(n).to_string(),
                reference_kind: kind,
                line: n.start_position().row as u32,
                column: n.start_position().column as u32,
                file_path: state.file_path.clone(),
            });
            return;
        }
        if n.kind() == "type_identifier" || n.kind() == "primitive_type" {
            state.unresolved_refs.push(UnresolvedRef {
                from_node_id: from_id.to_string(),
                reference_name: state.node_text(n).to_string(),
                reference_kind: kind,
                line: n.start_position().row as u32,
                column: n.start_position().column as u32,
                file_path: state.file_path.clone(),
            });
        }
        if cursor.goto_first_child() {
            loop {
                Self::emit_type_refs_walk(state, cursor, from_id, kind);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
    }

    /// Walk previous siblings of a declaration looking for `attribute_item` nodes
    /// and extract annotation usages from each one (skipping `derive` attributes,
    /// which are already handled by `extract_derive_macros`).
    fn extract_annotations_from_modifiers(
        state: &mut ExtractionState<'_>,
        node: TsNode<'_>,
        target_id: &str,
    ) {
        let mut current = node.prev_named_sibling();
        while let Some(sibling) = current {
            if sibling.kind() == "attribute_item" {
                let text = state.node_text(sibling);
                // Skip derive attributes. They are handled by extract_derive_macros.
                if !text.contains("derive") {
                    Self::extract_annotations_from_node(state, sibling, target_id);
                }
                current = sibling.prev_named_sibling();
            } else if sibling.kind() == "line_comment" || sibling.kind() == "block_comment" {
                current = sibling.prev_named_sibling();
            } else {
                break;
            }
        }
    }

    /// Create an `AnnotationUsage` node and edges for a single `attribute_item` node.
    fn extract_annotations_from_node(
        state: &mut ExtractionState<'_>,
        node: TsNode<'_>,
        target_id: &str,
    ) {
        let annot_name = Self::extract_annotation_name(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::@{}", state.qualified_prefix(), annot_name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::AnnotationUsage,
            &annot_name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::AnnotationUsage,
            name: annot_name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: Self::compute_attrs_start_line(node),
            end_line,
            start_column,
            end_column,
            signature: Some(state.node_text(node).trim().to_string()),
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

        state.unresolved_refs.push(UnresolvedRef {
            from_node_id: id.clone(),
            reference_name: annot_name,
            reference_kind: EdgeKind::Annotates,
            line: start_line,
            column: start_column,
            file_path: state.file_path.clone(),
        });

        state.edges.push(Edge {
            source: id,
            target: target_id.to_string(),
            kind: EdgeKind::Annotates,
            line: Some(start_line),
        });
    }

    /// Extract the name from a Rust `attribute_item` node.
    ///
    /// Trims `#[` and `]`, then takes everything before `(` as the name.
    /// E.g. `#[cfg(test)]` -> `cfg`, `#[inline]` -> `inline`.
    fn extract_annotation_name(state: &ExtractionState<'_>, node: TsNode<'_>) -> String {
        let text = state.node_text(node);
        let trimmed = text.trim();
        let inner = trimmed
            .strip_prefix("#[")
            .unwrap_or(trimmed)
            .strip_suffix(']')
            .unwrap_or(trimmed);
        inner.split('(').next().unwrap_or(inner).trim().to_string()
    }

    /// Build the graph and parser-backed import evidence accumulated in one traversal.
    fn build_artifact(state: ExtractionState<'_>, start: Instant) -> ExtractionArtifactV1 {
        ExtractionArtifactV1 {
            result: ExtractionResult {
                nodes: state.nodes,
                edges: state.edges,
                unresolved_refs: state.unresolved_refs,
                errors: state.errors,
                duration_ms: start.elapsed().as_millis() as u64,
            },
            imports: state.imports,
            clone_bodies: Vec::new(),
            schema_evidence: None,
        }
    }
}

impl crate::LanguageExtractor for RustExtractor {
    fn extensions(&self) -> &[&str] {
        &["rs"]
    }

    fn language_name(&self) -> &'static str {
        "Rust"
    }

    fn extract_parsed_artifact_prepared(
        &self,
        file_path: &str,
        source: &str,
        _parsed_source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        RustExtractor::extract_tree_artifact(file_path, source, tree, scope)
    }
}
