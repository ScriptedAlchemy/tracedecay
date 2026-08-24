use std::fmt::Debug;
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_application::CancellationSignal;
use tracedecay_application::retrieval::SymbolGraphScope;
use tracedecay_code_index::graph_projection::{
    CODE_GRAPH_PROJECTOR_REVISION, CodeGraphProjectionStore, CodeGraphSymbolBindingV1,
    build_code_graph_manifest, code_graph_projection_identity,
};
use tracedecay_code_index::lineage::LineageSymbolRecordV1;
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkV1, EdgeAuthorityV1,
    FileIdentityDigest, FileOccurrenceId, LanguageDescriptorRevision, PolicyRevisionId,
    RelationEdgeKindV1, SanitizerRevision, SensitivityDecision, SensitivityLevelV1, SourceSpan,
    SymbolIdentityDigest, SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GraphEntityId, GraphNamespace, GraphProjectorRevision, GraphProperty, GraphPropertyName,
    NeverCancelled, VerifiedGraphSnapshot,
};

use super::symbol_graph::trait_implementations;

#[test]
fn qualified_trait_selection_returns_only_its_stable_typed_implementors() {
    let store = store(true);
    let cancellation =
        CancellationSignal::active("primitive-implementation-selection").expect("cancellation");
    let reader = store
        .interactive_reader(&generation(), &cancellation)
        .expect("interactive reader");

    let records = trait_implementations(
        &reader,
        Arc::new(NeverCancelled),
        "src/storage/mod.rs::KeyValueStore",
        &SymbolGraphScope {
            path_prefix: Some("src/".to_owned()),
        },
    )
    .expect("typed implementation selection");

    assert_eq!(
        records
            .iter()
            .map(|record| (record.symbol.name.as_str(), record.symbol.kind.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("DiskStore", "impl"),
            ("MemoryStore", "impl"),
            ("ScopedStore", "impl"),
        ],
        "the exact trait identity must exclude an unrelated same-name trait and symbol"
    );
    assert!(
        records
            .windows(2)
            .all(|pair| { pair[0].symbol.node_id.as_str() < pair[1].symbol.node_id.as_str() }),
        "implementors must be stable by typed occurrence identity"
    );
}

#[test]
fn simple_trait_selection_uses_every_typed_same_name_identity() {
    let store = store(false);
    let cancellation = CancellationSignal::active("primitive-simple-implementation-selection")
        .expect("cancellation");
    let reader = store
        .interactive_reader(&generation(), &cancellation)
        .expect("interactive reader");
    let simple_records = trait_implementations(
        &reader,
        Arc::new(NeverCancelled),
        "KeyValueStore",
        &SymbolGraphScope { path_prefix: None },
    )
    .expect("simple typed implementation selection");
    assert_eq!(
        simple_records
            .iter()
            .map(|record| record.symbol.name.as_str())
            .collect::<Vec<_>>(),
        vec!["DiskStore", "ForeignStore", "MemoryStore"],
        "simple-name selection must retain all typed trait identities and ignore non-traits"
    );
}

fn store(with_scope_pressure: bool) -> CodeGraphProjectionStore {
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph").expect("namespace"))
            .expect("projection identity");
    let mut symbols = vec![
        symbol(
            "sym.trait.storage",
            "src/storage/mod.rs::KeyValueStore",
            "trait",
            '1',
        ),
        symbol("sym.trait.foreign", "foreign::KeyValueStore", "trait", '2'),
        symbol(
            "sym.function.same-name",
            "helpers::KeyValueStore",
            "function",
            '3',
        ),
        symbol(
            "sym.impl.memory",
            "src/storage/memory.rs::MemoryStore",
            "impl",
            '4',
        ),
        symbol(
            "sym.impl.disk",
            "src/storage/disk.rs::DiskStore",
            "impl",
            '5',
        ),
        symbol("sym.impl.foreign", "foreign::ForeignStore", "impl", '6'),
    ];
    let mut edges = vec![
        edge("sym.impl.memory", "sym.trait.storage", 2),
        edge("sym.impl.foreign", "sym.trait.foreign", 3),
        edge("sym.impl.disk", "sym.trait.storage", 1),
    ];
    if with_scope_pressure {
        symbols.push(symbol(
            "sym.impl.scoped",
            "src/storage/scoped.rs::ScopedStore",
            "impl",
            '7',
        ));
        edges.push(edge("sym.impl.scoped", "sym.trait.storage", 4));
        for ordinal in 0..201_u64 {
            let occurrence = format!("sym.impl.outside.{ordinal:03}");
            symbols.push(symbol(
                &occurrence,
                &format!("vendor/generated.rs::OutsideStore{ordinal:03}"),
                "impl",
                'a',
            ));
            edges.push(edge(&occurrence, "sym.trait.storage", ordinal + 10));
        }
    }
    let chunks = symbols
        .iter()
        .enumerate()
        .map(|(ordinal, symbol)| chunk(&symbol.occurrence, ordinal as u32))
        .collect::<Vec<_>>();
    let mut manifest = build_code_graph_manifest(
        projection,
        &generation(),
        &edges,
        &chunks,
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision"),
        Arc::new(NeverCancelled),
    )
    .expect("graph manifest");
    for (ordinal, symbol) in symbols.iter().enumerate() {
        let identity = GraphEntityId::new(stable_identity("symbol", symbol.occurrence.as_str()))
            .expect("symbol graph identity");
        let entity = manifest
            .entities
            .iter_mut()
            .find(|entity| entity.identity == identity)
            .expect("projected symbol entity");
        entity.properties.insert(
            GraphPropertyName::new("symbol-record").expect("symbol record property"),
            GraphProperty::Bytes(
                serde_json::to_vec(&SymbolRecordFixture {
                    occurrence: symbol.occurrence.clone(),
                    binding: Some(CodeGraphSymbolBindingV1 {
                        file: id("file.implementation.fixture"),
                        logical_path: Some(if symbol.qualified_name.starts_with("vendor/") {
                            "vendor/generated.rs".to_owned()
                        } else {
                            "src/storage.rs".to_owned()
                        }),
                        source_span: Some(SourceSpan {
                            start_byte: ordinal as u64,
                            end_byte: ordinal as u64 + 1,
                        }),
                        chunk: Some(id(&format!("chunk.implementation.{ordinal}"))),
                        language_descriptor_revision: id("language.rust.v1"),
                    }),
                    metadata: Some(symbol.clone()),
                })
                .expect("symbol record"),
            ),
        );
    }
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("verified snapshot");
    CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation())
        .expect("projection store")
}

fn generation() -> CodeGenerationId {
    id("generation.primitive.implementation.1")
}

fn symbol(
    occurrence: &str,
    qualified_name: &str,
    kind: &str,
    identity_byte: char,
) -> LineageSymbolRecordV1 {
    LineageSymbolRecordV1 {
        occurrence: id(occurrence),
        identity: digest::<SymbolIdentityDigest>(identity_byte),
        qualified_name: qualified_name.to_owned(),
        simple_name: qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(qualified_name)
            .to_owned(),
        kind: kind.to_owned(),
        visibility: "public".to_owned(),
        branches: 0,
        loops: 0,
        max_nesting: 0,
        line_span: 1,
        start_line: 0,
        signature: None,
        skip_test_coverage: false,
        file_identity: digest::<FileIdentityDigest>('e'),
        content_digest: digest('d'),
    }
}

fn chunk(symbol: &SymbolOccurrenceId, ordinal: u32) -> CodeSearchChunkV1 {
    CodeSearchChunkV1 {
        id: id(&format!("chunk.implementation.{ordinal}")),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation(),
            file_occurrence_id: id::<FileOccurrenceId>("file.implementation.fixture"),
            symbol_occurrence_id: Some(symbol.clone()),
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: u64::from(ordinal),
                end_byte: u64::from(ordinal) + 1,
            },
            grain: CodeSearchChunkGrainV1::SymbolBody,
            ordinal,
        },
        content_digest: digest('c'),
        language_descriptor_revision: id::<LanguageDescriptorRevision>("language.rust.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: id::<PolicyRevisionId>("policy.v1"),
        },
        exact_terms: Vec::new(),
        subtokens: Vec::new(),
        sanitized_text: BoundedSanitizedText::new("implementation fixture")
            .expect("bounded fixture text"),
    }
}

fn edge(from: &str, to: &str, start: u64) -> CanonicalRelationEdgeV1 {
    CanonicalRelationEdgeV1 {
        from_occurrence: id(from),
        to_occurrence: id(to),
        kind: RelationEdgeKindV1::Implements,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: start,
            end_byte: start + 1,
        },
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).expect("fixture identity")
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("fixture digest")
}

#[derive(Serialize)]
struct SymbolRecordFixture {
    occurrence: SymbolOccurrenceId,
    binding: Option<CodeGraphSymbolBindingV1>,
    metadata: Option<LineageSymbolRecordV1>,
}

fn stable_identity(kind: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("{kind}:{}", hex::encode(digest.finalize()))
}
