//! Graph node, edge, and extraction contracts shared across the workspace.
//!
//! Traversal, search, and context-assembly shapes that only the root façade
//! consumes live in `tracedecay::types` instead, so edits to them do not
//! invalidate every crate that depends on this one.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Declares a graph vocabulary enum and its persistence spelling in one place.
///
/// Each `Variant => "wire"` line is the sole authority for that variant: it
/// emits the enum variant, the `ALL` slot, the `as_str` arm, and the
/// `from_str` arm, so a spelling cannot drift between them. `as_str` stays a
/// direct exhaustive `match` — a new variant fails to compile until it is
/// declared here, and the node-ID hot path never scans the table. Extra
/// `| "alias"` spellings widen `from_str` only; `ALL` and `as_str` record what
/// is written. Serde representations come from the derives passed through on
/// the enum and are independent of these spellings.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident => $wire:literal $(| $alias:literal)*
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $( $(#[$variant_meta])* $variant, )+
        }

        #[allow(clippy::should_implement_trait)]
        impl $name {
            /// Every variant paired with the spelling [`Self::as_str`] emits and
            /// [`Self::from_str`] accepts, in declaration order. Inbound-only
            /// aliases are not listed: `ALL` records what is written.
            pub const ALL: [($name, &'static str); wire_enum!(@count $($variant)+)] =
                [$((Self::$variant, $wire),)+];

            pub const fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }

            pub fn from_str(s: &str) -> Option<Self> {
                match s {
                    $($wire $(| $alias)* => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
    (@count $($variant:ident)+) => {
        <[()]>::len(&[$(wire_enum!(@unit $variant)),+])
    };
    (@unit $variant:ident) => {
        ()
    };
}

wire_enum! {
    /// The persistence spelling is a contract, not a display detail: node IDs
    /// are `"{wire}:{hash}"` (see [`generate_node_id`]), so a renamed spelling
    /// invalidates every stored ID for that kind.
    #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum NodeKind {
        File => "file",
        Module => "module",
        Struct => "struct",
        Enum => "enum",
        EnumVariant => "enum_variant",
        Trait => "trait",
        Function => "function",
        Method => "method",
        Impl => "impl",
        Const => "const",
        Static => "static",
        TypeAlias => "type_alias",
        Field => "field",
        Macro => "macro",
        Use => "use",
        // Java-specific
        Class => "class",
        Interface => "interface",
        Constructor => "constructor",
        Annotation => "annotation",
        AnnotationUsage => "annotation_usage",
        Package => "package",
        InnerClass => "inner_class",
        InitBlock => "init_block",
        AbstractMethod => "abstract_method",
        // Go-specific
        InterfaceType => "interface_type",
        StructMethod => "struct_method",
        GoPackage => "go_package",
        StructTag => "struct_tag",
        // Scala-specific
        ScalaObject => "object",
        CaseClass => "case_class",
        ScalaPackage => "scala_package",
        ValField => "val",
        VarField => "var",
        // Shared
        GenericParam => "generic_param",
        // TypeScript/JavaScript-specific
        ArrowFunction => "arrow_function",
        Decorator => "decorator",
        Export => "export",
        Namespace => "namespace",
        // C/C++-specific
        Union => "union",
        Typedef => "typedef",
        Include => "include",
        PreprocessorDef => "preprocessor_def",
        Template => "template",
        // Kotlin-specific
        DataClass => "data_class",
        SealedClass => "sealed_class",
        CompanionObject => "companion_object",
        KotlinObject => "kotlin_object",
        KotlinPackage => "kotlin_package",
        Property => "property",
        // Dart-specific
        Mixin => "mixin",
        Extension => "extension",
        Library => "library",
        // C#-specific
        Delegate => "delegate",
        Event => "event",
        Record => "record",
        CSharpProperty => "csharp_property",
        // Pascal-specific
        Procedure => "procedure",
        PascalUnit => "pascal_unit",
        PascalProgram => "pascal_program",
        PascalRecord => "pascal_record",
        // Protobuf-specific. These are unconditional domain vocabulary; parser
        // availability remains a root-crate feature concern.
        ProtoMessage => "proto_message",
        ProtoService => "proto_service",
        ProtoRpc => "proto_rpc",
    }
}

impl NodeKind {
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

wire_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum EdgeKind {
        Contains => "contains",
        Calls => "calls",
        Uses => "uses",
        Implements => "implements",
        TypeOf => "type_of",
        Returns => "returns",
        DerivesMacro => "derives_macro",
        Extends => "extends",
        Annotates => "annotates",
        Receives => "receives",
    }
}

wire_enum! {
    #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum Visibility {
        // `"pub"` is accepted inbound only; `"public"` is what is written.
        Pub => "public" | "pub",
        PubCrate => "pub_crate",
        PubSuper => "pub_super",
        #[default]
        Private => "private",
    }
}

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

#[cfg(test)]
mod wire_spelling_tests {
    use std::fmt::Debug;

    use serde::Serialize;
    use serde_json::Value;

    use super::{EdgeKind, NodeKind, Visibility, generate_node_id};

    /// Every `ALL` slot must spell, parse, and serialize the same way it did
    /// before the spellings were declared once. Derived serde keeps the variant
    /// identifier — which is also what `Debug` prints for a unit variant — so
    /// the JSON contract is checked without a second literal inventory.
    fn assert_wire_round_trip<T: Debug + PartialEq + Serialize>(
        all: &[(T, &'static str)],
        as_str: fn(&T) -> &'static str,
        from_str: fn(&str) -> Option<T>,
    ) {
        for (kind, wire) in all {
            assert_eq!(as_str(kind), *wire, "{kind:?} no longer spells {wire:?}");
            assert_eq!(
                from_str(wire).as_ref(),
                Some(kind),
                "{wire:?} did not parse back to {kind:?}"
            );
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                Value::String(format!("{kind:?}")),
                "{kind:?} serde representation changed"
            );
        }
    }

    #[test]
    fn every_variant_round_trips_and_keeps_its_serde_spelling() {
        assert_wire_round_trip(&NodeKind::ALL, NodeKind::as_str, NodeKind::from_str);
        assert_wire_round_trip(&EdgeKind::ALL, EdgeKind::as_str, EdgeKind::from_str);
        assert_wire_round_trip(&Visibility::ALL, Visibility::as_str, Visibility::from_str);
    }

    /// Spellings that do not follow from the variant name, the inbound-only
    /// `"pub"` alias, and refusal of unknown spellings.
    #[test]
    fn representative_spellings_alias_and_refusal() {
        assert_eq!(NodeKind::ScalaObject.as_str(), "object");
        assert_eq!(NodeKind::ValField.as_str(), "val");
        assert_eq!(NodeKind::VarField.as_str(), "var");
        assert_eq!(NodeKind::EnumVariant.as_str(), "enum_variant");
        assert_eq!(EdgeKind::TypeOf.as_str(), "type_of");
        assert_eq!(Visibility::Pub.as_str(), "public");
        assert!(
            generate_node_id("Main.scala", &NodeKind::ScalaObject, "Main", 1)
                .starts_with("object:")
        );

        assert_eq!(Visibility::from_str("pub"), Some(Visibility::Pub));
        assert!(Visibility::ALL.iter().all(|(_, wire)| *wire != "pub"));
        assert_eq!(Visibility::default(), Visibility::Private);

        assert!(NodeKind::from_str("unknown_kind").is_none());
        assert!(NodeKind::from_str("").is_none());
        assert!(EdgeKind::from_str("unknown_edge").is_none());
        assert!(Visibility::from_str("unknown").is_none());
    }
}
