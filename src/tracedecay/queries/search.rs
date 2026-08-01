use std::collections::HashSet;

use crate::errors::Result;
use crate::tracedecay::TraceDecay;
use crate::types::*;

impl TraceDecay {
    /// Searches for nodes matching the given query string.
    ///
    /// Over-fetches from the FTS layer and re-ranks results so that symbol
    /// definitions (functions, structs, traits, etc.) sort above mere
    /// references (`use`, `module`, annotation usages) that happen to share
    /// the same name. BM25 alone does not distinguish kinds, so a `use foo`
    /// statement could outrank the actual `pub fn foo()` definition.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let overfetch = limit.saturating_mul(3).max(30);
        let trimmed_query = query.trim();
        let mut raw = self.db.search_nodes(query, overfetch).await?;

        // FTS/BM25 can bury exact symbol definitions below many short import
        // rows. On Sonium, `LinearOperator` had dozens of `use ...LinearOperator`
        // rows in the top FTS window while the actual trait definition was
        // outside `overfetch`, so the kind tier below never saw it. Seed the
        // candidate set with exact `name = query` hits first, then dedup.
        if !trimmed_query.is_empty() {
            let mut exact_names = vec![trimmed_query.to_string()];
            if let Some(short) = trimmed_query.rsplit("::").next()
                && short != trimmed_query
                && !short.is_empty()
            {
                exact_names.push(short.to_string());
            }
            let exact = self
                .db
                .search_nodes_by_exact_name(&exact_names, overfetch)
                .await?;
            raw.extend(
                exact
                    .into_iter()
                    .map(|node| SearchResult { node, score: 0.0 }),
            );
        }

        let mut seen = HashSet::new();
        let mut ranked: Vec<SearchResult> = raw
            .into_iter()
            .filter(|r| seen.insert(r.node.id.clone()))
            .map(|mut r| {
                r.score += kind_rank_bonus(&r.node.kind);
                // Exact-name match boost: when the node's `name` equals the
                // query verbatim, surface it ahead of partial / qualified-name
                // matches. Without this, searching for a trait like
                // `LinearOperator` could be outranked by a `Method` whose
                // qualified name happens to contain `LinearOperator` (e.g.
                // a method declared inside the trait body), or by a `Field`
                // that shares the same simple name.
                if !trimmed_query.is_empty() && r.node.name == trimmed_query {
                    r.score += 10.0;
                }
                r
            })
            .collect();
        // Sort by kind tier first (definitions > references), then score
        // descending. Tier-first avoids any chance that a `use` re-export
        // (kind tier = `Use`) outscores a real definition because BM25
        // happened to weight the short re-export row highly. Score is the
        // secondary key so within a tier we still respect BM25.
        ranked.sort_by(|a, b| {
            kind_tier(&a.node.kind)
                .cmp(&kind_tier(&b.node.kind))
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        ranked.truncate(limit);
        Ok(ranked)
    }
}

/// Coarse ranking tier (primary sort key) and rank bonus (score adjustment)
/// used together in `search`. Merged into one exhaustive match so the two
/// partitions of `NodeKind` — previously duplicated across `kind_tier` and
/// `kind_rank_bonus` — can't drift apart.
///
/// Tiers separate "real definitions" (functions, types, traits, …) from
/// "references" (`use`, `module`, annotation usage) so a re-export can never
/// beat the thing it re-exports, no matter what BM25 produces for the row.
/// Lower tier numbers sort first. The bonus is added directly to the BM25
/// score so a definition with a slightly worse BM25 score still surfaces
/// above its imports.
///
/// Exhaustive match by design: when a new `NodeKind` variant is added the
/// compiler will force a re-tune here rather than silently defaulting it to
/// `(0, 0.0)`, matching the project rule "crash hard if there is an unknown
/// value".
fn kind_rank(kind: &NodeKind) -> (u8, f64) {
    match kind {
        // Tier 0 / callable definitions — the "what is this?" answers a
        // user usually wants when searching by symbol name.
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::StructMethod
        | NodeKind::Constructor
        | NodeKind::AbstractMethod
        | NodeKind::ArrowFunction
        | NodeKind::Procedure => (0, 3.0),
        // Tier 0 / type definitions.
        NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Class
        | NodeKind::InnerClass
        | NodeKind::Interface
        | NodeKind::InterfaceType
        | NodeKind::Record
        | NodeKind::CaseClass
        | NodeKind::DataClass
        | NodeKind::SealedClass
        | NodeKind::TypeAlias
        | NodeKind::Union
        | NodeKind::Typedef
        | NodeKind::Mixin
        | NodeKind::Extension
        | NodeKind::Delegate
        | NodeKind::Template
        | NodeKind::PascalRecord
        | NodeKind::ScalaObject
        | NodeKind::KotlinObject
        | NodeKind::CompanionObject
        | NodeKind::Annotation
        | NodeKind::Event => (0, 2.5),
        // Tier 0 / proto definitions are unconditional domain vocabulary
        // even when the parser implementation is not enabled in this build.
        NodeKind::ProtoMessage | NodeKind::ProtoService | NodeKind::ProtoRpc => (0, 2.5),
        // Tier 1: impl blocks — between definitions and references.
        NodeKind::Impl => (1, 2.0),
        // Tier 2 / values, macros, preprocessor defs.
        NodeKind::Const
        | NodeKind::Static
        | NodeKind::Macro
        | NodeKind::PreprocessorDef
        | NodeKind::EnumVariant => (2, 1.0),
        // Tier 2 / members of types.
        NodeKind::Field
        | NodeKind::ValField
        | NodeKind::VarField
        | NodeKind::Property
        | NodeKind::CSharpProperty
        | NodeKind::StructTag
        | NodeKind::InitBlock
        | NodeKind::Export => (2, 0.5),
        // Tier 3 / file & generic-parameter — neutral bonus.
        NodeKind::File | NodeKind::GenericParam | NodeKind::PascalProgram => (3, 0.0),
        // Tier 3 / containers (module, namespace, …) — usually not the
        // answer to "find symbol".
        NodeKind::Module
        | NodeKind::Package
        | NodeKind::Namespace
        | NodeKind::ScalaPackage
        | NodeKind::GoPackage
        | NodeKind::KotlinPackage
        | NodeKind::PascalUnit
        | NodeKind::Library => (3, -1.5),
        // Tier 4 / pure references — always rank last.
        NodeKind::Use | NodeKind::Include => (4, -3.0),
        NodeKind::AnnotationUsage | NodeKind::Decorator => (4, -2.0),
    }
}

/// Thin wrapper over [`kind_rank`] for callers that only need the sort tier.
fn kind_tier(kind: &NodeKind) -> u8 {
    kind_rank(kind).0
}

/// Thin wrapper over [`kind_rank`] for callers that only need the score bonus.
fn kind_rank_bonus(kind: &NodeKind) -> f64 {
    kind_rank(kind).1
}

/// True when the user-supplied query matches either the node's short `name`
/// or its `qualified_name`. Matching is exact on the short name and substring
/// on the qualified name, so callers can pass either form for the impl/trait
/// filter on `tracedecay_impls`.
pub(super) fn node_name_matches(node: &Node, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    node.name == query || node.qualified_name == query || node.qualified_name.contains(query)
}
