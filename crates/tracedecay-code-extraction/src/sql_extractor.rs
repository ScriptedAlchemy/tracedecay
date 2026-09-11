use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tree_sitter::{Node as TsNode, Tree};

use crate::common::local_node_id;
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef,
    Visibility, generate_node_id,
};
use crate::{
    ExtractedSchemaEvidenceV1, ExtractedSchemaFactV1, ExtractionArtifactV1, SchemaEvidenceIssueV1,
    SchemaEvidenceLanguageV1, SchemaEvidenceStatusV1, SqlSchemaActionV1, SqlSchemaObjectKindV1,
};

pub struct SqlExtractor;

struct ExtractionState<'s> {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_refs: Vec<UnresolvedRef>,
    errors: Vec<String>,
    schema_facts: Vec<ExtractedSchemaFactV1>,
    schema_issues: Vec<SchemaEvidenceIssueV1>,
    statement_order: u32,
    file_path: String,
    source: &'s [u8],
    file_node_id: String,
    timestamp: u64,
}

impl<'s> ExtractionState<'s> {
    fn new(file_path: &str, source: &'s str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let file_node_id = generate_node_id(file_path, &NodeKind::File, file_path, 0);
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_refs: Vec::new(),
            errors: Vec::new(),
            schema_facts: Vec::new(),
            schema_issues: Vec::new(),
            statement_order: 0,
            file_path: file_path.to_string(),
            source: source.as_bytes(),
            file_node_id,
            timestamp,
        }
    }

    fn node_text(&self, node: TsNode<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("<invalid utf8>")
    }
}

impl SqlExtractor {
    pub fn extract_sql(file_path: &str, source: &str) -> ExtractionResult {
        Self::extract_sql_artifact(file_path, source).result
    }

    fn extract_sql_artifact(file_path: &str, source: &str) -> ExtractionArtifactV1 {
        let tree = match Self::parse_source(source) {
            Ok(t) => t,
            Err(msg) => {
                let start = Instant::now();
                let mut state = ExtractionState::new(file_path, source);
                state.errors.push(msg);
                state.schema_issues.push(SchemaEvidenceIssueV1::ParseError);
                return Self::build_artifact(state, start);
            }
        };
        Self::extract_tree_artifact(
            file_path,
            source,
            &tree,
            crate::parsed_extraction::ParsedExtractionScope::FullDocument,
        )
        .artifact
    }

    fn extract_tree(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        Self::extract_tree_artifact(file_path, source, tree, scope).into_parsed()
    }

    fn extract_tree_artifact(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        let start = Instant::now();
        let mut state = ExtractionState::new(file_path, source);
        if tree.root_node().has_error() {
            state.schema_issues.push(SchemaEvidenceIssueV1::ParseError);
        }

        let file_node = Node {
            id: state.file_node_id.clone(),
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
        state.nodes.push(file_node);

        let metrics = crate::parsed_extraction::visit_root_children(tree, scope, |child| {
            Self::visit_node(&mut state, child);
        });

        crate::parsed_extraction::ParsedExtractionArtifactV1::complete(
            Self::build_artifact(state, start),
            scope,
            metrics,
        )
    }

    fn parse_source(source: &str) -> Result<Tree, String> {
        crate::ts_provider::parse_extractor_source("sql", "SQL", source)
    }

    fn visit_children(state: &mut ExtractionState, node: TsNode<'_>) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                Self::visit_node(state, cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn visit_node(state: &mut ExtractionState, node: TsNode<'_>) {
        match node.kind() {
            "statement" => Self::visit_statement(state, node),
            "create_table" => Self::emit_schema_change(
                state,
                node,
                SqlSchemaActionV1::Create,
                SqlSchemaObjectKindV1::Table,
                NodeKind::Class,
            ),
            "alter_table" => Self::emit_schema_change(
                state,
                node,
                SqlSchemaActionV1::Alter,
                SqlSchemaObjectKindV1::Table,
                NodeKind::Class,
            ),
            "drop_table" => Self::emit_schema_change(
                state,
                node,
                SqlSchemaActionV1::Drop,
                SqlSchemaObjectKindV1::Table,
                NodeKind::Class,
            ),
            "create_view" | "create_materialized_view" => Self::emit_schema_change(
                state,
                node,
                SqlSchemaActionV1::Create,
                SqlSchemaObjectKindV1::View,
                NodeKind::Class,
            ),
            "alter_view" => Self::emit_schema_change(
                state,
                node,
                SqlSchemaActionV1::Alter,
                SqlSchemaObjectKindV1::View,
                NodeKind::Class,
            ),
            "drop_view" => Self::emit_schema_change(
                state,
                node,
                SqlSchemaActionV1::Drop,
                SqlSchemaObjectKindV1::View,
                NodeKind::Class,
            ),
            "create_function" => Self::emit_schema_change(
                state,
                node,
                SqlSchemaActionV1::Create,
                Self::routine_kind(node),
                NodeKind::Function,
            ),
            "drop_function" => Self::emit_schema_change(
                state,
                node,
                SqlSchemaActionV1::Drop,
                Self::routine_kind(node),
                NodeKind::Function,
            ),
            "keyword_execute" => state
                .schema_issues
                .push(SchemaEvidenceIssueV1::UnsupportedSyntax),
            "ERROR" => {
                let first_token = state
                    .node_text(node)
                    .split_ascii_whitespace()
                    .next()
                    .unwrap_or_default();
                state.schema_issues.push(
                    if first_token.eq_ignore_ascii_case("execute")
                        || first_token.eq_ignore_ascii_case("exec")
                    {
                        SchemaEvidenceIssueV1::UnsupportedSyntax
                    } else {
                        SchemaEvidenceIssueV1::ParseError
                    },
                );
            }
            _ => Self::visit_children(state, node),
        }
    }

    fn visit_statement(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let action = if has_direct_child(node, "keyword_create") {
            Some(SqlSchemaActionV1::Create)
        } else if has_direct_child(node, "keyword_alter") {
            Some(SqlSchemaActionV1::Alter)
        } else if has_direct_child(node, "keyword_drop") {
            Some(SqlSchemaActionV1::Drop)
        } else {
            None
        };
        let object_kind = if has_direct_child(node, "keyword_table") {
            Some((SqlSchemaObjectKindV1::Table, NodeKind::Class))
        } else if has_direct_child(node, "keyword_view") {
            Some((SqlSchemaObjectKindV1::View, NodeKind::Class))
        } else if has_direct_child(node, "keyword_function") {
            Some((SqlSchemaObjectKindV1::Function, NodeKind::Function))
        } else if has_direct_child(node, "keyword_procedure") {
            Some((SqlSchemaObjectKindV1::Procedure, NodeKind::Function))
        } else {
            None
        };
        match (action, object_kind) {
            (Some(action), Some((object_kind, graph_kind))) => {
                Self::emit_schema_change(state, node, action, object_kind, graph_kind);
            }
            _ => Self::visit_children(state, node),
        }
    }

    fn emit_schema_change(
        state: &mut ExtractionState<'_>,
        node: TsNode<'_>,
        action: SqlSchemaActionV1,
        object_kind: SqlSchemaObjectKindV1,
        graph_kind: NodeKind,
    ) {
        let statement_order = state.statement_order;
        state.statement_order = state.statement_order.saturating_add(1);
        match Self::extract_qualified_object_name(state, node) {
            Some(qualified_name) => {
                state
                    .schema_facts
                    .push(ExtractedSchemaFactV1::SqlObjectChange {
                        statement_order,
                        action,
                        object_kind,
                        qualified_name,
                        span: source_span(node),
                    });
            }
            None => state
                .schema_issues
                .push(SchemaEvidenceIssueV1::DynamicIdentity),
        }
        Self::emit_named(state, node, graph_kind);
    }

    fn routine_kind(node: TsNode<'_>) -> SqlSchemaObjectKindV1 {
        if contains_kind(node, "keyword_procedure") {
            SqlSchemaObjectKindV1::Procedure
        } else {
            SqlSchemaObjectKindV1::Function
        }
    }

    /// Extracts the object name from an `object_reference` child and emits a node.
    fn emit_named(state: &mut ExtractionState, node: TsNode<'_>, kind: NodeKind) {
        let name = Self::extract_object_name(state, node)
            .unwrap_or_else(|| format!("<anonymous_{}>", node.kind()));

        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let text = state.node_text(node);
        let sig = text.lines().next().map(|l| l.trim().to_string());
        let qualified_name = format!("{}::{}", state.file_path, name);
        let id = local_node_id(&state.file_path, state.source, &kind, &name, node);

        let graph_node = Node {
            id: id.clone(),
            kind,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column: node.start_position().column as u32,
            end_column: node.end_position().column as u32,
            signature: sig,
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

        state.edges.push(Edge {
            source: state.file_node_id.clone(),
            target: id,
            kind: EdgeKind::Contains,
            line: Some(start_line),
        });
    }

    /// Walks the node looking for an `object_reference` child, returns its text.
    fn extract_object_name(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        Self::extract_qualified_object_name(state, node).map(|qualified| {
            qualified
                .split('.')
                .next_back()
                .unwrap_or(&qualified)
                .to_owned()
        })
    }

    fn extract_qualified_object_name(
        state: &ExtractionState<'_>,
        node: TsNode<'_>,
    ) -> Option<String> {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "object_reference" {
                    let name = state.node_text(child).trim();
                    return (!name.is_empty()).then(|| name.to_owned());
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        None
    }

    fn build_artifact(state: ExtractionState<'_>, start: Instant) -> ExtractionArtifactV1 {
        let status = if state.schema_issues.is_empty() {
            SchemaEvidenceStatusV1::Complete
        } else if state.schema_facts.is_empty()
            && state
                .schema_issues
                .contains(&SchemaEvidenceIssueV1::UnsupportedSyntax)
        {
            SchemaEvidenceStatusV1::Unsupported
        } else {
            SchemaEvidenceStatusV1::Partial
        };
        let mut artifact = ExtractionArtifactV1 {
            result: ExtractionResult {
                nodes: state.nodes,
                edges: state.edges,
                unresolved_refs: state.unresolved_refs,
                errors: state.errors,
                duration_ms: start.elapsed().as_millis() as u64,
            },
            imports: Vec::new(),
            schema_evidence: Some(ExtractedSchemaEvidenceV1 {
                logical_path: state.file_path,
                language: SchemaEvidenceLanguageV1::Sql,
                status,
                issues: state.schema_issues,
                facts: state.schema_facts,
            }),
        };
        artifact.canonicalize_order();
        artifact
    }
}

fn source_span(node: TsNode<'_>) -> tracedecay_domain::SourceSpan {
    tracedecay_domain::SourceSpan {
        start_byte: node.start_byte() as u64,
        end_byte: node.end_byte() as u64,
    }
}

fn has_direct_child(node: TsNode<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| child.kind() == kind)
}

fn contains_kind(node: TsNode<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| contains_kind(child, kind))
}

impl crate::LanguageExtractor for SqlExtractor {
    fn extensions(&self) -> &[&str] {
        &["sql"]
    }

    fn language_name(&self) -> &'static str {
        "SQL"
    }

    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        Self::extract_sql(file_path, source)
    }

    fn extract_artifact(&self, file_path: &str, source: &str) -> ExtractionArtifactV1 {
        Self::extract_sql_artifact(file_path, source)
    }

    fn extract_parsed(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        Self::extract_tree(file_path, source, tree, scope)
    }

    fn extract_parsed_artifact(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        if matches!(
            scope,
            crate::parsed_extraction::ParsedExtractionScope::ChangedRegions(_)
        ) {
            let full = Self::extract_tree_artifact(
                file_path,
                source,
                tree,
                crate::parsed_extraction::ParsedExtractionScope::FullDocument,
            );
            return crate::parsed_extraction::ParsedExtractionArtifactV1::reset(
                full.artifact,
                crate::parsed_extraction::ParsedExtractionResetReason::ChangedRootIdentity,
                source.len(),
            );
        }
        Self::extract_tree_artifact(file_path, source, tree, scope)
    }
}
