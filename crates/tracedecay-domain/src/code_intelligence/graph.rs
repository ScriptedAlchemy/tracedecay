//! Graph node, edge, and extraction contracts shared across the workspace.
//!
//! Traversal, search, and context-assembly shapes that only the root façade
//! consumes live in `tracedecay::types` instead, so edits to them do not
//! invalidate every crate that depends on this one.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Byte-wise `&str` equality usable during `const` evaluation, where
/// `PartialEq` is not. Only the `ALL` totality guards in this module call it.
const fn same_wire_str(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    File,
    Module,
    Struct,
    Enum,
    EnumVariant,
    Trait,
    Function,
    Method,
    Impl,
    Const,
    Static,
    TypeAlias,
    Field,
    Macro,
    Use,
    // Java-specific
    Class,
    Interface,
    Constructor,
    Annotation,
    AnnotationUsage,
    Package,
    InnerClass,
    InitBlock,
    AbstractMethod,
    // Go-specific
    InterfaceType,
    StructMethod,
    GoPackage,
    StructTag,
    // Scala-specific
    ScalaObject,
    CaseClass,
    ScalaPackage,
    ValField,
    VarField,
    // Shared
    GenericParam,
    // TypeScript/JavaScript-specific
    ArrowFunction,
    Decorator,
    Export,
    Namespace,
    // C/C++-specific
    Union,
    Typedef,
    Include,
    PreprocessorDef,
    Template,
    // Kotlin-specific
    DataClass,
    SealedClass,
    CompanionObject,
    KotlinObject,
    KotlinPackage,
    Property,
    // Dart-specific
    Mixin,
    Extension,
    Library,
    // C#-specific
    Delegate,
    Event,
    Record,
    CSharpProperty,
    // Pascal-specific
    Procedure,
    PascalUnit,
    PascalProgram,
    PascalRecord,
    // Protobuf-specific. These are unconditional domain vocabulary; parser
    // availability remains a root-crate feature concern.
    ProtoMessage,
    ProtoService,
    ProtoRpc,
}

#[allow(clippy::should_implement_trait)]
impl NodeKind {
    /// Every variant paired with the wire string [`NodeKind::as_str`] emits and
    /// [`NodeKind::from_str`] accepts.
    ///
    /// The pairing is a persistence contract, not a display detail: node IDs
    /// are `"{wire}:{hash}"` (see [`generate_node_id`]), so a renamed string
    /// invalidates every stored ID for that kind. Callers that need to iterate
    /// or exhaustively test the kind space should drive off this table rather
    /// than hand-maintaining a list. [`NodeKind::wire_str_from_all`] keeps it
    /// total.
    pub const ALL: [(NodeKind, &'static str); 63] = [
        (Self::File, "file"),
        (Self::Module, "module"),
        (Self::Struct, "struct"),
        (Self::Enum, "enum"),
        (Self::EnumVariant, "enum_variant"),
        (Self::Trait, "trait"),
        (Self::Function, "function"),
        (Self::Method, "method"),
        (Self::Impl, "impl"),
        (Self::Const, "const"),
        (Self::Static, "static"),
        (Self::TypeAlias, "type_alias"),
        (Self::Field, "field"),
        (Self::Macro, "macro"),
        (Self::Use, "use"),
        (Self::Class, "class"),
        (Self::Interface, "interface"),
        (Self::Constructor, "constructor"),
        (Self::Annotation, "annotation"),
        (Self::AnnotationUsage, "annotation_usage"),
        (Self::Package, "package"),
        (Self::InnerClass, "inner_class"),
        (Self::InitBlock, "init_block"),
        (Self::AbstractMethod, "abstract_method"),
        (Self::InterfaceType, "interface_type"),
        (Self::StructMethod, "struct_method"),
        (Self::GoPackage, "go_package"),
        (Self::StructTag, "struct_tag"),
        (Self::ScalaObject, "object"),
        (Self::CaseClass, "case_class"),
        (Self::ScalaPackage, "scala_package"),
        (Self::ValField, "val"),
        (Self::VarField, "var"),
        (Self::GenericParam, "generic_param"),
        (Self::ArrowFunction, "arrow_function"),
        (Self::Decorator, "decorator"),
        (Self::Export, "export"),
        (Self::Namespace, "namespace"),
        (Self::Union, "union"),
        (Self::Typedef, "typedef"),
        (Self::Include, "include"),
        (Self::PreprocessorDef, "preprocessor_def"),
        (Self::Template, "template"),
        (Self::DataClass, "data_class"),
        (Self::SealedClass, "sealed_class"),
        (Self::CompanionObject, "companion_object"),
        (Self::KotlinObject, "kotlin_object"),
        (Self::KotlinPackage, "kotlin_package"),
        (Self::Property, "property"),
        (Self::Mixin, "mixin"),
        (Self::Extension, "extension"),
        (Self::Library, "library"),
        (Self::Delegate, "delegate"),
        (Self::Event, "event"),
        (Self::Record, "record"),
        (Self::CSharpProperty, "csharp_property"),
        (Self::Procedure, "procedure"),
        (Self::PascalUnit, "pascal_unit"),
        (Self::PascalProgram, "pascal_program"),
        (Self::PascalRecord, "pascal_record"),
        (Self::ProtoMessage, "proto_message"),
        (Self::ProtoService, "proto_service"),
        (Self::ProtoRpc, "proto_rpc"),
    ];

    /// Compile-time totality proof for [`NodeKind::ALL`]. Never called at
    /// runtime; it exists so the table cannot silently fall behind the enum.
    ///
    /// The match is exhaustive, so a new variant does not compile until it is
    /// named here, and each arm has to name a real `ALL` slot — the slot count
    /// is part of `ALL`'s type, so the natural next index does not compile
    /// until the variant is also appended to `ALL`. The `const` block below
    /// then rejects any arm that points at the wrong slot.
    ///
    /// [`NodeKind::as_str`] keeps its own literals instead of delegating here:
    /// it is on the node-ID hot path and must not depend on indexing a
    /// 63-entry table.
    const fn wire_str_from_all(&self) -> &'static str {
        match self {
            Self::File => Self::ALL[0].1,
            Self::Module => Self::ALL[1].1,
            Self::Struct => Self::ALL[2].1,
            Self::Enum => Self::ALL[3].1,
            Self::EnumVariant => Self::ALL[4].1,
            Self::Trait => Self::ALL[5].1,
            Self::Function => Self::ALL[6].1,
            Self::Method => Self::ALL[7].1,
            Self::Impl => Self::ALL[8].1,
            Self::Const => Self::ALL[9].1,
            Self::Static => Self::ALL[10].1,
            Self::TypeAlias => Self::ALL[11].1,
            Self::Field => Self::ALL[12].1,
            Self::Macro => Self::ALL[13].1,
            Self::Use => Self::ALL[14].1,
            Self::Class => Self::ALL[15].1,
            Self::Interface => Self::ALL[16].1,
            Self::Constructor => Self::ALL[17].1,
            Self::Annotation => Self::ALL[18].1,
            Self::AnnotationUsage => Self::ALL[19].1,
            Self::Package => Self::ALL[20].1,
            Self::InnerClass => Self::ALL[21].1,
            Self::InitBlock => Self::ALL[22].1,
            Self::AbstractMethod => Self::ALL[23].1,
            Self::InterfaceType => Self::ALL[24].1,
            Self::StructMethod => Self::ALL[25].1,
            Self::GoPackage => Self::ALL[26].1,
            Self::StructTag => Self::ALL[27].1,
            Self::ScalaObject => Self::ALL[28].1,
            Self::CaseClass => Self::ALL[29].1,
            Self::ScalaPackage => Self::ALL[30].1,
            Self::ValField => Self::ALL[31].1,
            Self::VarField => Self::ALL[32].1,
            Self::GenericParam => Self::ALL[33].1,
            Self::ArrowFunction => Self::ALL[34].1,
            Self::Decorator => Self::ALL[35].1,
            Self::Export => Self::ALL[36].1,
            Self::Namespace => Self::ALL[37].1,
            Self::Union => Self::ALL[38].1,
            Self::Typedef => Self::ALL[39].1,
            Self::Include => Self::ALL[40].1,
            Self::PreprocessorDef => Self::ALL[41].1,
            Self::Template => Self::ALL[42].1,
            Self::DataClass => Self::ALL[43].1,
            Self::SealedClass => Self::ALL[44].1,
            Self::CompanionObject => Self::ALL[45].1,
            Self::KotlinObject => Self::ALL[46].1,
            Self::KotlinPackage => Self::ALL[47].1,
            Self::Property => Self::ALL[48].1,
            Self::Mixin => Self::ALL[49].1,
            Self::Extension => Self::ALL[50].1,
            Self::Library => Self::ALL[51].1,
            Self::Delegate => Self::ALL[52].1,
            Self::Event => Self::ALL[53].1,
            Self::Record => Self::ALL[54].1,
            Self::CSharpProperty => Self::ALL[55].1,
            Self::Procedure => Self::ALL[56].1,
            Self::PascalUnit => Self::ALL[57].1,
            Self::PascalProgram => Self::ALL[58].1,
            Self::PascalRecord => Self::ALL[59].1,
            Self::ProtoMessage => Self::ALL[60].1,
            Self::ProtoService => Self::ALL[61].1,
            Self::ProtoRpc => Self::ALL[62].1,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Module => "module",
            NodeKind::Struct => "struct",
            NodeKind::Enum => "enum",
            NodeKind::EnumVariant => "enum_variant",
            NodeKind::Trait => "trait",
            NodeKind::Function => "function",
            NodeKind::Method => "method",
            NodeKind::Impl => "impl",
            NodeKind::Const => "const",
            NodeKind::Static => "static",
            NodeKind::TypeAlias => "type_alias",
            NodeKind::Field => "field",
            NodeKind::Macro => "macro",
            NodeKind::Use => "use",
            NodeKind::Class => "class",
            NodeKind::Interface => "interface",
            NodeKind::Constructor => "constructor",
            NodeKind::Annotation => "annotation",
            NodeKind::AnnotationUsage => "annotation_usage",
            NodeKind::Package => "package",
            NodeKind::InnerClass => "inner_class",
            NodeKind::InitBlock => "init_block",
            NodeKind::AbstractMethod => "abstract_method",
            NodeKind::InterfaceType => "interface_type",
            NodeKind::StructMethod => "struct_method",
            NodeKind::GoPackage => "go_package",
            NodeKind::StructTag => "struct_tag",
            NodeKind::ScalaObject => "object",
            NodeKind::CaseClass => "case_class",
            NodeKind::ScalaPackage => "scala_package",
            NodeKind::ValField => "val",
            NodeKind::VarField => "var",
            NodeKind::GenericParam => "generic_param",
            NodeKind::ArrowFunction => "arrow_function",
            NodeKind::Decorator => "decorator",
            NodeKind::Export => "export",
            NodeKind::Namespace => "namespace",
            NodeKind::Union => "union",
            NodeKind::Typedef => "typedef",
            NodeKind::Include => "include",
            NodeKind::PreprocessorDef => "preprocessor_def",
            NodeKind::Template => "template",
            NodeKind::DataClass => "data_class",
            NodeKind::SealedClass => "sealed_class",
            NodeKind::CompanionObject => "companion_object",
            NodeKind::KotlinObject => "kotlin_object",
            NodeKind::KotlinPackage => "kotlin_package",
            NodeKind::Property => "property",
            NodeKind::Mixin => "mixin",
            NodeKind::Extension => "extension",
            NodeKind::Library => "library",
            NodeKind::Delegate => "delegate",
            NodeKind::Event => "event",
            NodeKind::Record => "record",
            NodeKind::CSharpProperty => "csharp_property",
            NodeKind::Procedure => "procedure",
            NodeKind::PascalUnit => "pascal_unit",
            NodeKind::PascalProgram => "pascal_program",
            NodeKind::PascalRecord => "pascal_record",
            NodeKind::ProtoMessage => "proto_message",
            NodeKind::ProtoService => "proto_service",
            NodeKind::ProtoRpc => "proto_rpc",
        }
    }

    pub fn from_str(s: &str) -> Option<NodeKind> {
        match s {
            "file" => Some(NodeKind::File),
            "module" => Some(NodeKind::Module),
            "struct" => Some(NodeKind::Struct),
            "enum" => Some(NodeKind::Enum),
            "enum_variant" => Some(NodeKind::EnumVariant),
            "trait" => Some(NodeKind::Trait),
            "function" => Some(NodeKind::Function),
            "method" => Some(NodeKind::Method),
            "impl" => Some(NodeKind::Impl),
            "const" => Some(NodeKind::Const),
            "static" => Some(NodeKind::Static),
            "type_alias" => Some(NodeKind::TypeAlias),
            "field" => Some(NodeKind::Field),
            "macro" => Some(NodeKind::Macro),
            "use" => Some(NodeKind::Use),
            "class" => Some(NodeKind::Class),
            "interface" => Some(NodeKind::Interface),
            "constructor" => Some(NodeKind::Constructor),
            "annotation" => Some(NodeKind::Annotation),
            "annotation_usage" => Some(NodeKind::AnnotationUsage),
            "package" => Some(NodeKind::Package),
            "inner_class" => Some(NodeKind::InnerClass),
            "init_block" => Some(NodeKind::InitBlock),
            "abstract_method" => Some(NodeKind::AbstractMethod),
            "interface_type" => Some(NodeKind::InterfaceType),
            "struct_method" => Some(NodeKind::StructMethod),
            "go_package" => Some(NodeKind::GoPackage),
            "struct_tag" => Some(NodeKind::StructTag),
            "object" => Some(NodeKind::ScalaObject),
            "case_class" => Some(NodeKind::CaseClass),
            "scala_package" => Some(NodeKind::ScalaPackage),
            "val" => Some(NodeKind::ValField),
            "var" => Some(NodeKind::VarField),
            "generic_param" => Some(NodeKind::GenericParam),
            "arrow_function" => Some(NodeKind::ArrowFunction),
            "decorator" => Some(NodeKind::Decorator),
            "export" => Some(NodeKind::Export),
            "namespace" => Some(NodeKind::Namespace),
            "union" => Some(NodeKind::Union),
            "typedef" => Some(NodeKind::Typedef),
            "include" => Some(NodeKind::Include),
            "preprocessor_def" => Some(NodeKind::PreprocessorDef),
            "template" => Some(NodeKind::Template),
            "data_class" => Some(NodeKind::DataClass),
            "sealed_class" => Some(NodeKind::SealedClass),
            "companion_object" => Some(NodeKind::CompanionObject),
            "kotlin_object" => Some(NodeKind::KotlinObject),
            "kotlin_package" => Some(NodeKind::KotlinPackage),
            "property" => Some(NodeKind::Property),
            "mixin" => Some(NodeKind::Mixin),
            "extension" => Some(NodeKind::Extension),
            "library" => Some(NodeKind::Library),
            "delegate" => Some(NodeKind::Delegate),
            "event" => Some(NodeKind::Event),
            "record" => Some(NodeKind::Record),
            "csharp_property" => Some(NodeKind::CSharpProperty),
            "procedure" => Some(NodeKind::Procedure),
            "pascal_unit" => Some(NodeKind::PascalUnit),
            "pascal_program" => Some(NodeKind::PascalProgram),
            "pascal_record" => Some(NodeKind::PascalRecord),
            "proto_message" => Some(NodeKind::ProtoMessage),
            "proto_service" => Some(NodeKind::ProtoService),
            "proto_rpc" => Some(NodeKind::ProtoRpc),
            _ => None,
        }
    }

    /// Returns `true` if this node kind represents a callable definition that
    /// should participate in test-coverage / attribution accounting.
    ///
    /// This includes free functions and methods across all languages, plus
    /// TypeScript/JavaScript arrow functions (`const f = () => {}`), which are
    /// the dominant way tests and helpers are written in TS test suites. Without
    /// arrow functions the TS coverage denominators silently exclude most of the
    /// callable universe.
    pub fn is_callable_kind(&self) -> bool {
        matches!(
            self,
            NodeKind::Function | NodeKind::Method | NodeKind::ArrowFunction
        )
    }
}

/// Rejects a [`NodeKind::wire_str_from_all`] arm that points at the wrong
/// [`NodeKind::ALL`] slot, which is the only way a variant could be named in
/// the totality match yet be absent from (or misplaced in) the table.
const _: () = {
    let mut slot = 0;
    while slot < NodeKind::ALL.len() {
        assert!(
            same_wire_str(
                NodeKind::ALL[slot].0.wire_str_from_all(),
                NodeKind::ALL[slot].1
            ),
            "NodeKind::wire_str_from_all points at the wrong NodeKind::ALL slot"
        );
        slot += 1;
    }
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    Contains,
    Calls,
    Uses,
    Implements,
    TypeOf,
    Returns,
    DerivesMacro,
    Extends,
    Annotates,
    Receives,
}

#[allow(clippy::should_implement_trait)]
impl EdgeKind {
    /// Every variant paired with the wire string [`EdgeKind::as_str`] emits and
    /// [`EdgeKind::from_str`] accepts. Kept total by
    /// [`EdgeKind::wire_str_from_all`], exactly as [`NodeKind::ALL`] is.
    pub const ALL: [(EdgeKind, &'static str); 10] = [
        (Self::Contains, "contains"),
        (Self::Calls, "calls"),
        (Self::Uses, "uses"),
        (Self::Implements, "implements"),
        (Self::TypeOf, "type_of"),
        (Self::Returns, "returns"),
        (Self::DerivesMacro, "derives_macro"),
        (Self::Extends, "extends"),
        (Self::Annotates, "annotates"),
        (Self::Receives, "receives"),
    ];

    /// Compile-time totality proof for [`EdgeKind::ALL`]; see
    /// [`NodeKind::wire_str_from_all`] for how the guard works.
    const fn wire_str_from_all(&self) -> &'static str {
        match self {
            Self::Contains => Self::ALL[0].1,
            Self::Calls => Self::ALL[1].1,
            Self::Uses => Self::ALL[2].1,
            Self::Implements => Self::ALL[3].1,
            Self::TypeOf => Self::ALL[4].1,
            Self::Returns => Self::ALL[5].1,
            Self::DerivesMacro => Self::ALL[6].1,
            Self::Extends => Self::ALL[7].1,
            Self::Annotates => Self::ALL[8].1,
            Self::Receives => Self::ALL[9].1,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Contains => "contains",
            EdgeKind::Calls => "calls",
            EdgeKind::Uses => "uses",
            EdgeKind::Implements => "implements",
            EdgeKind::TypeOf => "type_of",
            EdgeKind::Returns => "returns",
            EdgeKind::DerivesMacro => "derives_macro",
            EdgeKind::Extends => "extends",
            EdgeKind::Annotates => "annotates",
            EdgeKind::Receives => "receives",
        }
    }

    pub fn from_str(s: &str) -> Option<EdgeKind> {
        match s {
            "contains" => Some(EdgeKind::Contains),
            "calls" => Some(EdgeKind::Calls),
            "uses" => Some(EdgeKind::Uses),
            "implements" => Some(EdgeKind::Implements),
            "type_of" => Some(EdgeKind::TypeOf),
            "returns" => Some(EdgeKind::Returns),
            "derives_macro" => Some(EdgeKind::DerivesMacro),
            "extends" => Some(EdgeKind::Extends),
            "annotates" => Some(EdgeKind::Annotates),
            "receives" => Some(EdgeKind::Receives),
            _ => None,
        }
    }
}

/// Rejects an [`EdgeKind::wire_str_from_all`] arm that points at the wrong
/// [`EdgeKind::ALL`] slot.
const _: () = {
    let mut slot = 0;
    while slot < EdgeKind::ALL.len() {
        assert!(
            same_wire_str(
                EdgeKind::ALL[slot].0.wire_str_from_all(),
                EdgeKind::ALL[slot].1
            ),
            "EdgeKind::wire_str_from_all points at the wrong EdgeKind::ALL slot"
        );
        slot += 1;
    }
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Visibility {
    Pub,
    PubCrate,
    PubSuper,
    #[default]
    Private,
}

impl Visibility {
    /// Every variant paired with the wire string [`Visibility::as_str`] emits.
    /// [`Visibility::from_str`] also accepts `"pub"` as an inbound alias for
    /// `"public"`, which is deliberately not part of this table: `ALL` records
    /// what is written, the alias only widens what is read. Kept total by
    /// [`Visibility::wire_str_from_all`], as [`NodeKind::ALL`] is.
    pub const ALL: [(Visibility, &'static str); 4] = [
        (Self::Pub, "public"),
        (Self::PubCrate, "pub_crate"),
        (Self::PubSuper, "pub_super"),
        (Self::Private, "private"),
    ];

    /// Compile-time totality proof for [`Visibility::ALL`]; see
    /// [`NodeKind::wire_str_from_all`] for how the guard works.
    const fn wire_str_from_all(&self) -> &'static str {
        match self {
            Self::Pub => Self::ALL[0].1,
            Self::PubCrate => Self::ALL[1].1,
            Self::PubSuper => Self::ALL[2].1,
            Self::Private => Self::ALL[3].1,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pub => "public",
            Self::PubCrate => "pub_crate",
            Self::PubSuper => "pub_super",
            Self::Private => "private",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "public" | "pub" => Some(Self::Pub),
            "pub_crate" => Some(Self::PubCrate),
            "pub_super" => Some(Self::PubSuper),
            "private" => Some(Self::Private),
            _ => None,
        }
    }
}

/// Rejects a [`Visibility::wire_str_from_all`] arm that points at the wrong
/// [`Visibility::ALL`] slot.
const _: () = {
    let mut slot = 0;
    while slot < Visibility::ALL.len() {
        assert!(
            same_wire_str(
                Visibility::ALL[slot].0.wire_str_from_all(),
                Visibility::ALL[slot].1
            ),
            "Visibility::wire_str_from_all points at the wrong Visibility::ALL slot"
        );
        slot += 1;
    }
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub start_line: u32,
    /// First line of the leading doc-comment / attribute block, or `start_line`
    /// when no such block exists. Lets refactoring tools select the full span
    /// of an item (delete, move, rewrite) without losing its documentation.
    pub attrs_start_line: u32,
    pub end_line: u32,
    pub start_column: u32,
    pub end_column: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub visibility: Visibility,
    pub is_async: bool,
    /// Number of branching statements (if, match/switch arms, ternary).
    /// 0 for non-function nodes. Cyclomatic complexity = branches + 1.
    pub branches: u32,
    /// Number of loop constructs (for, while, loop).
    pub loops: u32,
    /// Number of early-exit statements (return, break, continue, throw).
    pub returns: u32,
    /// Maximum brace nesting depth within the function body.
    pub max_nesting: u32,
    /// Number of unsafe blocks/statements within the function body.
    pub unsafe_blocks: u32,
    /// Number of unchecked/force-unwrap calls (e.g. `.unwrap()`, `!!`, `.get()` on Optional).
    pub unchecked_calls: u32,
    /// Number of assertion calls (e.g. `assert!`, `assertEquals`, `expect`).
    pub assertions: u32,
    pub updated_at: u64,
    /// `id` of the enclosing scope (module, impl, class, …). `None` for
    /// top-level nodes whose parent is the file itself. Populated from
    /// `Contains` edges at insert time; once written, callers should prefer
    /// `parent_id` over walking edges.
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub kind: EdgeKind,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedRef {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: EdgeKind,
    pub line: u32,
    pub column: u32,
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved_refs: Vec<UnresolvedRef>,
    pub errors: Vec<String>,
    pub duration_ms: u64,
}

impl ExtractionResult {
    /// Strip nodes with empty names and remove any edges or unresolved refs
    /// that reference their IDs. Tree-sitter can produce empty-name nodes
    /// from complex declarators (especially C/C++); if we skip the node at
    /// insert time but keep its edges, we get FK constraint violations.
    pub fn sanitize(&mut self) {
        let before = self.nodes.len();
        let bad_ids: HashSet<String> = self
            .nodes
            .iter()
            .filter(|n| n.name.is_empty())
            .map(|n| n.id.clone())
            .collect();

        if bad_ids.is_empty() {
            return;
        }

        self.nodes.retain(|n| !n.name.is_empty());
        self.edges
            .retain(|e| !bad_ids.contains(&e.source) && !bad_ids.contains(&e.target));
        self.unresolved_refs
            .retain(|r| !bad_ids.contains(&r.from_node_id));

        let removed = before - self.nodes.len();
        if removed > 0 {
            self.errors
                .push(format!("stripped {removed} node(s) with empty names"));
        }
    }

    /// Deterministic canonical row order shared by full-document and
    /// incremental extraction, so identical content serializes byte-identically
    /// regardless of traversal path: file rows first, then source position with
    /// enclosing (larger) spans before their children, with the content-hash id
    /// as the final total-order tiebreaker.
    pub fn canonicalize_order(&mut self) {
        self.nodes.sort_by(|left, right| {
            let left_is_file = left.kind == NodeKind::File;
            let right_is_file = right.kind == NodeKind::File;
            right_is_file
                .cmp(&left_is_file)
                .then_with(|| left.start_line.cmp(&right.start_line))
                .then_with(|| left.start_column.cmp(&right.start_column))
                .then_with(|| right.end_line.cmp(&left.end_line))
                .then_with(|| right.end_column.cmp(&left.end_column))
                .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.edges.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.target.cmp(&right.target))
                .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
        });
        self.unresolved_refs.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| left.column.cmp(&right.column))
                .then_with(|| left.from_node_id.cmp(&right.from_node_id))
                .then_with(|| left.reference_name.cmp(&right.reference_name))
                .then_with(|| {
                    left.reference_kind
                        .as_str()
                        .cmp(right.reference_kind.as_str())
                })
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub node_count: u64,
    pub edge_count: u64,
    pub file_count: u64,
    pub nodes_by_kind: HashMap<String, u64>,
    pub edges_by_kind: HashMap<String, u64>,
    pub db_size_bytes: u64,
    pub last_updated: u64,
    /// Total bytes of all indexed source files.
    pub total_source_bytes: u64,
    /// Number of indexed files per language (e.g. "Rust" -> 42).
    pub files_by_language: HashMap<String, u64>,
    /// Timestamp of the most recent incremental sync (0 if never synced).
    pub last_sync_at: u64,
    /// Timestamp of the most recent full (re)index (0 if never indexed).
    pub last_full_sync_at: u64,
    /// Duration in milliseconds of the most recent sync (0 if unknown).
    pub last_sync_duration_ms: u64,
}

/// Generates a deterministic node ID from file path, kind, name, and line number.
///
/// The ID format is `"kind:32hexchars"` where the hex portion is the first 32
/// characters of the SHA-256 hash of the input components.
/// Extracted names may be empty for anonymous source constructs; file, kind,
/// and line keep those identities deterministic and distinct.
pub fn generate_node_id(file_path: &str, kind: &NodeKind, name: &str, line: u32) -> String {
    let input = format!("{}:{}:{}:{}", file_path, kind.as_str(), name, line);
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let hash = hasher.finalize();
    let hex_str = crate::canonical_text::encode_lowercase_hex(&hash);
    format!("{}:{}", kind.as_str(), &hex_str[..32])
}

#[cfg(test)]
mod empty_name_node_id_tests {
    use super::{NodeKind, generate_node_id};

    #[test]
    fn empty_name_yields_a_deterministic_id_in_every_profile() {
        let first = generate_node_id(
            "integration/fs-routes-test.ts",
            &NodeKind::Function,
            "",
            286,
        );
        let second = generate_node_id(
            "integration/fs-routes-test.ts",
            &NodeKind::Function,
            "",
            286,
        );

        assert_eq!(first, second, "empty-name ids must be deterministic");
        assert!(
            first.starts_with("function:"),
            "unexpected id shape: {first}"
        );
    }

    #[test]
    fn empty_name_ids_stay_distinct_per_file_kind_and_line() {
        let base = generate_node_id("a.ts", &NodeKind::Function, "", 286);

        assert_ne!(base, generate_node_id("b.ts", &NodeKind::Function, "", 286));
        assert_ne!(base, generate_node_id("a.ts", &NodeKind::Class, "", 286));
        assert_ne!(base, generate_node_id("a.ts", &NodeKind::Function, "", 287));
    }
}
