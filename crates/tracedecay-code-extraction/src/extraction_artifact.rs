//! Parser-backed evidence emitted alongside the legacy extraction graph.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{ExtractionResult, SourceSpan};

use crate::ExtractedCloneBodyV1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SchemaEvidenceLanguageV1 {
    Protobuf,
    Sql,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SchemaEvidenceStatusV1 {
    Complete,
    Partial,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SchemaEvidenceIssueV1 {
    DynamicIdentity,
    MigrationOrderUnknown,
    ParseError,
    SourceTruncated,
    UnsupportedSyntax,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SqlSchemaActionV1 {
    Alter,
    Create,
    Drop,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SqlSchemaObjectKindV1 {
    Function,
    Procedure,
    Table,
    View,
}

/// One parser-observed schema identity or ordered migration operation.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtractedSchemaFactV1 {
    ProtobufMessage {
        qualified_name: String,
        span: SourceSpan,
    },
    ProtobufField {
        message_qualified_name: String,
        name: String,
        type_name: String,
        tag: u32,
        span: SourceSpan,
    },
    ProtobufService {
        qualified_name: String,
        span: SourceSpan,
    },
    ProtobufRpc {
        service_qualified_name: String,
        name: String,
        request_type: String,
        response_type: String,
        span: SourceSpan,
    },
    SqlObjectChange {
        statement_order: u32,
        action: SqlSchemaActionV1,
        object_kind: SqlSchemaObjectKindV1,
        qualified_name: String,
        span: SourceSpan,
    },
}

impl ExtractedSchemaFactV1 {
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::ProtobufMessage { span, .. }
            | Self::ProtobufField { span, .. }
            | Self::ProtobufService { span, .. }
            | Self::ProtobufRpc { span, .. }
            | Self::SqlObjectChange { span, .. } => *span,
        }
    }
}

/// File-scoped schema evidence from the same parser traversal as graph extraction.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExtractedSchemaEvidenceV1 {
    pub logical_path: String,
    pub language: SchemaEvidenceLanguageV1,
    pub status: SchemaEvidenceStatusV1,
    pub issues: Vec<SchemaEvidenceIssueV1>,
    pub facts: Vec<ExtractedSchemaFactV1>,
}

impl ExtractedSchemaEvidenceV1 {
    pub(crate) fn canonicalize_order(&mut self) {
        self.issues.sort();
        self.issues.dedup();
        self.facts.sort();
        self.facts.dedup();
    }

    pub fn mark_partial(&mut self, issue: SchemaEvidenceIssueV1) {
        if self.status == SchemaEvidenceStatusV1::Complete {
            self.status = SchemaEvidenceStatusV1::Partial;
        }
        self.issues.push(issue);
        self.canonicalize_order();
    }
}

/// Semantic namespace occupied by one imported binding.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImportNamespaceV1 {
    Type,
    Value,
    SideEffect,
}

/// Resolution boundary implied by a source module specifier.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImportModuleKindV1 {
    BareModule,
    ProjectRelative,
}

/// Classify a module specifier using the importing language's syntax.
pub fn import_module_kind(language: &str, module_specifier: &str) -> Option<ImportModuleKindV1> {
    match language {
        "rust" => Some(rust_import_module_kind(module_specifier)),
        "typescript" | "tsx" | "javascript" | "astro" | "svelte" | "go" | "ruby" => {
            Some(path_import_module_kind(module_specifier))
        }
        "python" => Some(if module_specifier.starts_with('.') {
            ImportModuleKindV1::ProjectRelative
        } else {
            ImportModuleKindV1::BareModule
        }),
        "java" => Some(ImportModuleKindV1::BareModule),
        _ => None,
    }
}

pub(crate) fn rust_import_module_kind(module_specifier: &str) -> ImportModuleKindV1 {
    let project_relative = matches!(module_specifier, "crate" | "self" | "super")
        || module_specifier.starts_with("crate::")
        || module_specifier.starts_with("self::")
        || module_specifier.starts_with("super::");
    if project_relative {
        ImportModuleKindV1::ProjectRelative
    } else {
        ImportModuleKindV1::BareModule
    }
}

/// A path specifier is project-relative only when it starts at the importing
/// file's directory; Ruby's `require_relative` is recorded in that form.
pub(crate) fn path_import_module_kind(module_specifier: &str) -> ImportModuleKindV1 {
    if matches!(module_specifier, "." | "..")
        || module_specifier.starts_with("./")
        || module_specifier.starts_with("../")
    {
        ImportModuleKindV1::ProjectRelative
    } else {
        ImportModuleKindV1::BareModule
    }
}

/// Scope that a restricted Rust re-export exposes.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case", tag = "kind", content = "module")]
pub enum ImportReexportScopeV1 {
    Crate,
    Super,
    SelfModule,
    Module(String),
}

/// One parser-backed import binding or side-effect statement.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(deny_unknown_fields)]
pub struct ExtractedImportEvidenceV1 {
    pub logical_path: String,
    pub module_specifier: String,
    pub imported_name: Option<String>,
    pub local_name: Option<String>,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reexport_scope: Option<ImportReexportScopeV1>,
    #[serde(default)]
    pub is_glob: bool,
    pub namespace: ImportNamespaceV1,
    pub module_kind: ImportModuleKindV1,
    pub span: SourceSpan,
    pub start_line: u32,
    pub start_column: u32,
}

/// The binding one private import row records.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ImportBindingV1<'a> {
    /// `imported` bound under `local` (`from m import a as b`).
    Named { imported: &'a str, local: &'a str },
    /// The whole module bound under `local` (`import m as b`, a Go package).
    Namespace { local: &'a str },
    /// Every public name of the module (`from m import *`, `import m.*`).
    Glob,
    /// A load with no binding (`require_relative "m"`, `import _ "m"`).
    SideEffect,
}

impl ExtractedImportEvidenceV1 {
    /// One private import row attested by the parser node `evidence`.
    pub(crate) fn private_binding(
        logical_path: &str,
        language: &str,
        module_specifier: &str,
        binding: ImportBindingV1<'_>,
        namespace: ImportNamespaceV1,
        evidence: tree_sitter::Node<'_>,
    ) -> Result<Self, String> {
        let module_kind = import_module_kind(language, module_specifier)
            .ok_or_else(|| format!("{language} import rows have no module classification"))?;
        let span_error = || format!("{language} import span exceeds canonical span width");
        let span = SourceSpan {
            start_byte: u64::try_from(evidence.start_byte()).map_err(|_| span_error())?,
            end_byte: u64::try_from(evidence.end_byte()).map_err(|_| span_error())?,
        };
        let start = evidence.start_position();
        let (imported_name, local_name, is_glob, namespace) = match binding {
            ImportBindingV1::Named { imported, local } => {
                (Some(imported), Some(local), false, namespace)
            }
            ImportBindingV1::Namespace { local } => (Some("*"), Some(local), false, namespace),
            ImportBindingV1::Glob => (Some("*"), None, true, namespace),
            ImportBindingV1::SideEffect => (None, None, false, ImportNamespaceV1::SideEffect),
        };
        Ok(Self {
            logical_path: logical_path.to_owned(),
            module_specifier: module_specifier.to_owned(),
            imported_name: imported_name.map(str::to_owned),
            local_name: local_name.map(str::to_owned),
            is_public: false,
            reexport_scope: None,
            is_glob,
            namespace,
            module_kind,
            span,
            start_line: u32::try_from(start.row).map_err(|_| span_error())?,
            start_column: u32::try_from(start.column).map_err(|_| span_error())?,
        })
    }
}

impl Ord for ExtractedImportEvidenceV1 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.logical_path
            .cmp(&other.logical_path)
            .then_with(|| self.span.cmp(&other.span))
            .then_with(|| self.module_specifier.cmp(&other.module_specifier))
            .then_with(|| self.imported_name.cmp(&other.imported_name))
            .then_with(|| self.local_name.cmp(&other.local_name))
            .then_with(|| self.is_public.cmp(&other.is_public))
            .then_with(|| self.reexport_scope.cmp(&other.reexport_scope))
            .then_with(|| self.is_glob.cmp(&other.is_glob))
            .then_with(|| self.namespace.cmp(&other.namespace))
            .then_with(|| self.module_kind.cmp(&other.module_kind))
            .then_with(|| self.start_line.cmp(&other.start_line))
            .then_with(|| self.start_column.cmp(&other.start_column))
    }
}

impl PartialOrd for ExtractedImportEvidenceV1 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The declared parameter list of a callable: `parameters` formal
/// parameters, the last a variable-arity `T...` one when `variadic`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct CallableArityV1 {
    pub parameters: u32,
    pub variadic: bool,
}

impl CallableArityV1 {
    /// Whether a call passing `arguments` arguments can invoke this callable.
    pub fn accepts(self, arguments: u32) -> bool {
        if self.variadic {
            arguments.saturating_add(1) >= self.parameters
        } else {
            arguments == self.parameters
        }
    }
}

/// The parameter list of the callable node `node_id`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ExtractedCallableArityV1 {
    pub node_id: String,
    pub arity: CallableArityV1,
}

/// Legacy graph extraction plus structured evidence from the same traversal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionArtifactV1 {
    pub result: ExtractionResult,
    pub imports: Vec<ExtractedImportEvidenceV1>,
    pub clone_bodies: Vec<ExtractedCloneBodyV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_evidence: Option<ExtractedSchemaEvidenceV1>,
    /// Parameter lists of the callables whose extractor records them, so a
    /// call binds the overload that accepts its arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callable_arities: Vec<ExtractedCallableArityV1>,
}

impl ExtractionArtifactV1 {
    pub(crate) fn from_result(result: ExtractionResult) -> Self {
        Self {
            result,
            imports: Vec::new(),
            clone_bodies: Vec::new(),
            schema_evidence: None,
            callable_arities: Vec::new(),
        }
    }

    /// A graph extraction with the import rows the same traversal attested.
    pub(crate) fn with_imports(
        result: ExtractionResult,
        imports: Vec<ExtractedImportEvidenceV1>,
    ) -> Self {
        Self {
            imports,
            ..Self::from_result(result)
        }
    }

    pub(crate) fn canonicalize_order(&mut self) {
        self.result.canonicalize_order();
        self.imports.sort();
        crate::clone_body::canonicalize_clone_body_order(&mut self.clone_bodies);
        self.callable_arities.sort();
        if let Some(evidence) = &mut self.schema_evidence {
            evidence.canonicalize_order();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CallableArityV1, ImportModuleKindV1, import_module_kind};

    #[test]
    fn a_variadic_callable_accepts_an_empty_or_longer_tail() {
        let fixed = CallableArityV1 {
            parameters: 2,
            variadic: false,
        };
        let variadic = CallableArityV1 {
            parameters: 2,
            variadic: true,
        };
        assert_eq!(
            (0..4).map(|n| fixed.accepts(n)).collect::<Vec<_>>(),
            [false, false, true, false]
        );
        assert_eq!(
            (0..4).map(|n| variadic.accepts(n)).collect::<Vec<_>>(),
            [false, true, true, true]
        );
    }

    #[test]
    fn module_kind_uses_the_importing_language_syntax() {
        for specifier in [
            "crate",
            "crate::target",
            "self",
            "self::target",
            "super",
            "super::target",
        ] {
            assert_eq!(
                import_module_kind("rust", specifier),
                Some(ImportModuleKindV1::ProjectRelative)
            );
        }
        assert_eq!(
            import_module_kind("typescript", "crate"),
            Some(ImportModuleKindV1::BareModule)
        );
        assert_eq!(
            import_module_kind("typescript", "../target"),
            Some(ImportModuleKindV1::ProjectRelative)
        );
    }
}
