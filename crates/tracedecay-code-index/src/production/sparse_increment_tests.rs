//! A successor sealed over its parent must seal what a cold build of the
//! same tree seals: the same file segments and graph pages, the same
//! statistics, coverage, and resolution index, and the same restored edges
//! and call limitations.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use sha2::Digest as _;
use tracedecay_domain::{LanguageId, ProjectionKeyV1, ProjectionKindV1, SanitizationReceiptId};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessResidentSampleV1, ResidentMemoryPressureV1,
    detected_process_resident_memory_limit_v1,
};

use super::module_resolution::ModuleImportIndexV1;
use super::resolution_view::SymbolsByNameV1;
use super::sealed_parent::SealedParentGenerationV1;
use super::sparse_resolution::{SparseDefinitionWindowV1, SparseSymbolsByNameV1};
use super::typescript_resolution::ImportBindingOutcomeV1;
use super::worker_tests::{PanicOnFileWorker, worker_config, worker_id};
use super::*;

const RUST: &str = "rust";

#[test]
fn typescript_shadowed_local_writes_preserve_sealed_module_receivers() {
    const CALLER: &str = r#"import { Worker } from './workers';
const worker = new Worker();
function shadow() {
  let worker: Worker = new Worker();
  worker = new Worker();
}
export function stable() { worker.execute(); }
let captured: Worker = new Worker();
function replace() { captured = new Worker(); }
export function reassigned() { captured.execute(); }
"#;
    let store = MemorySealedPublicationStoreV1::default();
    publish(
        &mut owner(&store),
        &[
            ("main.ts", "typescript", CALLER),
            (
                "workers.ts",
                "typescript",
                "export class Worker { execute() {} }\nexport class Alternate { execute() {} }\n",
            ),
        ],
        1_100_000,
    );
    let restored = store
        .decode_active(&scope())
        .expect("sealed receiver context decodes")
        .expect("published generation");
    for (caller_name, reference, expected) in [
        (
            "main.ts::stable",
            "worker.execute",
            Some("workers.ts::Worker::execute"),
        ),
        ("main.ts::reassigned", "captured.execute", None),
    ] {
        let caller = &restored
            .symbols()
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == caller_name)
            .expect("fixture caller")
            .occurrence;
        let calls = restored
            .edges()
            .iter()
            .filter(|edge| {
                edge.from_occurrence == *caller && edge.kind == RelationEdgeKindV1::Calls
            })
            .collect::<Vec<_>>();
        if let Some(expected) = expected {
            assert_eq!(calls.len(), 1, "{caller_name}");
            let edge = calls[0];
            assert_eq!(
                edge.authority,
                tracedecay_domain::EdgeAuthorityV1::NameResolved
            );
            assert_eq!(
                &CALLER
                    [edge.evidence_span.start_byte as usize..edge.evidence_span.end_byte as usize],
                reference
            );
            assert_eq!(
                restored
                    .symbols()
                    .symbols
                    .iter()
                    .find(|symbol| symbol.occurrence == edge.to_occurrence)
                    .expect("resolved target")
                    .qualified_name,
                expected
            );
        } else {
            assert!(calls.is_empty(), "a written captured binding must abstain");
        }
        assert_eq!(
            restored
                .unresolved_import_calls()
                .expect("restored unresolved receiver evidence")
                .iter()
                .any(|gap| gap.from_occurrence == *caller && gap.reference_name == reference),
            expected.is_none(),
            "{caller_name}"
        );
    }
}

#[test]
fn typescript_factory_return_edits_rebind_unchanged_receivers_after_restore() {
    const CALLER: &str = "import { makeWorker as acquire } from './factory';\nclass Created { execute() {} }\nexport async function run() { const worker = await acquire(); const alias = worker; alias.execute(); }\n";
    const FACTORY: &str = "import { Worker as Created, Alternate as Changed } from './workers';\nexport async function makeWorker(): Promise<Created> { return new Created(); }\n";
    fn tree(factory: &str) -> Vec<(&str, &str, &str)> {
        vec![
            ("main.ts", "typescript", CALLER),
            ("factory.ts", "typescript", factory),
            (
                "workers.ts",
                "typescript",
                "export class Worker { execute() {} }\nexport class Alternate { execute() {} }\n",
            ),
            ("a.ts", "typescript", "export const a = 1;"),
            ("b.ts", "typescript", "export const b = 1;"),
            ("c.ts", "typescript", "export const c = 1;"),
            ("d.ts", "typescript", "export const d = 1;"),
            ("e.ts", "typescript", "export const e = 1;"),
            ("f.ts", "typescript", "export const f = 1;"),
        ]
    }
    fn assert_receiver(generation: &CodeIndexPublishedGenerationV1, target: Option<&str>) {
        let caller = &generation
            .symbols()
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "main.ts::run")
            .unwrap()
            .occurrence;
        let edges = generation
            .edges()
            .iter()
            .filter(|edge| {
                edge.from_occurrence == *caller
                    && edge.kind == RelationEdgeKindV1::Calls
                    && generation.symbols().symbols.iter().any(|symbol| {
                        symbol.occurrence == edge.to_occurrence && symbol.simple_name == "execute"
                    })
            })
            .collect::<Vec<_>>();
        if let Some(target) = target {
            assert_eq!(edges.len(), 1);
            let edge = edges[0];
            assert_eq!(
                edge.authority,
                tracedecay_domain::EdgeAuthorityV1::NameResolved
            );
            assert_eq!(
                &CALLER
                    [edge.evidence_span.start_byte as usize..edge.evidence_span.end_byte as usize],
                "alias.execute"
            );
            assert_eq!(
                generation
                    .symbols()
                    .symbols
                    .iter()
                    .find(|symbol| symbol.occurrence == edge.to_occurrence)
                    .unwrap()
                    .qualified_name,
                target
            );
            assert!(
                !generation
                    .unresolved_import_calls()
                    .unwrap()
                    .iter()
                    .any(|gap| gap.from_occurrence == *caller
                        && gap.reference_name == "alias.execute")
            );
        } else {
            assert!(edges.is_empty());
            assert!(
                generation
                    .unresolved_import_calls()
                    .unwrap()
                    .iter()
                    .any(|gap| gap.from_occurrence == *caller
                        && gap.reference_name == "alias.execute")
            );
        }
    }
    let store = MemorySealedPublicationStoreV1::default();
    publish(&mut owner(&store), &tree(FACTORY), 1_100_000);
    let restored = store.decode_active(&scope()).unwrap().unwrap();
    assert_receiver(&restored, Some("workers.ts::Worker::execute"));
    let caller_file = restored
        .files
        .iter()
        .find(|file| file.authority.logical_path == "main.ts")
        .unwrap()
        .extraction
        .file_occurrence_id
        .clone();
    drop(restored);

    let changed = FACTORY
        .replace("Promise<Created>", "Promise<Changed>")
        .replace("new Created()", "new Changed()");
    let after = owner(&store)
        .build_and_publish(
            request_capturing(&tree(&changed), &["factory.ts"], 1_200_000),
            &UninterruptibleCodeIndexControlV1,
        )
        .unwrap();
    assert_eq!(
        after.cold_reason(),
        None,
        "only the factory file is re-extracted"
    );
    let restored = store.decode_active(&scope()).unwrap().unwrap();
    assert_receiver(&restored, Some("workers.ts::Alternate::execute"));
    assert_eq!(
        restored
            .files
            .iter()
            .find(|file| file.authority.logical_path == "main.ts")
            .unwrap()
            .extraction
            .file_occurrence_id,
        caller_file
    );
    let cold_store = MemorySealedPublicationStoreV1::default();
    publish(&mut owner(&cold_store), &tree(&changed), 1_200_000);
    assert_eq!(answers(&store), answers(&cold_store));
    drop(restored);

    let unsupported = changed.replace("Promise<Changed>", "Promise<Created | Changed>");
    let after = owner(&store)
        .build_and_publish(
            request_capturing(&tree(&unsupported), &["factory.ts"], 1_300_000),
            &UninterruptibleCodeIndexControlV1,
        )
        .unwrap();
    assert_eq!(after.cold_reason(), None);
    assert_receiver(&store.decode_active(&scope()).unwrap().unwrap(), None);
}

fn occurrence(path: &str, source: &[u8]) -> FileOccurrenceId {
    let digest = sha2::Sha256::digest([path.as_bytes(), b"\0", source].concat());
    worker_id(&format!(
        "file.sparse.{}",
        digest
            .iter()
            .take(12)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn request_capturing(
    files: &[(&str, &str, &str)],
    capture: &[&str],
    sealed_at: i64,
) -> CodeIndexBuildRequestV1 {
    let mut request = request(files, sealed_at);
    request.captured_files.retain(|captured| {
        request.snapshot.files.iter().any(|file| {
            file.file_occurrence_id == captured.file_occurrence_id
                && capture.contains(&file.logical_path.as_str())
        })
    });
    request.changed_files = capture.iter().map(|path| (*path).to_owned()).collect();
    request
}

pub(super) fn request(files: &[(&str, &str, &str)], sealed_at: i64) -> CodeIndexBuildRequestV1 {
    let mut rows = files
        .iter()
        .map(|(path, language, source)| SanitizedCodeFileV1 {
            file_occurrence_id: occurrence(path, source.as_bytes()),
            logical_path: (*path).to_owned(),
            language: Some(worker_id::<LanguageId>(language)),
            content_digest: content_digest(source.as_bytes()),
            disposition: SnapshotFileDispositionV1::Present,
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (&left.logical_path, &left.file_occurrence_id)
            .cmp(&(&right.logical_path, &right.file_occurrence_id))
    });
    let identity = files
        .iter()
        .flat_map(|(path, _, source)| [path.as_bytes(), source.as_bytes()])
        .collect::<Vec<_>>()
        .concat();
    CodeIndexBuildRequestV1 {
        resident_memory: Arc::new(ProcessResidentMemoryV1::new(
            detected_process_resident_memory_limit_v1(),
        )),
        snapshot: SanitizedCodeSnapshotV1 {
            repository: worker_id("repository.worker"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: worker_id("sanitizer.v1"),
            sanitization_receipts: vec![worker_id::<SanitizationReceiptId>("receipt.worker")],
            content_identity: content_digest(&identity),
            captured_at: UtcMicros(1_000_000),
            files: rows,
            omitted_sources: Vec::new(),
        },
        captured_files: files
            .iter()
            .map(|(path, _, source)| CodeIndexCapturedFileV1 {
                file_occurrence_id: occurrence(path, source.as_bytes()),
                sanitized_bytes: Arc::from(source.as_bytes()),
                sensitivity_level: SensitivityLevelV1::Public,
            })
            .collect(),
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            include_submodules: false,
            tree: None,
            dirty: tracedecay_domain::RepositoryDirtyStateV1::Dirty,
        },
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.v1".to_owned(),
            profile_digest: worker_id(&format!("sha256:{}", "a".repeat(64))),
        },
        sealed_at: UtcMicros(sealed_at),
    }
}

type Owner = CodeIndexProductionOwnerV1<MemorySealedPublicationStoreV1>;

pub(super) fn owner(store: &MemorySealedPublicationStoreV1) -> Owner {
    CodeIndexProductionOwnerV1::new(worker_config(), store.clone()).expect("production owner")
}

fn publish(
    owner: &mut Owner,
    files: &[(&str, &str, &str)],
    sealed_at: i64,
) -> CodeIndexPublishedBuildV1 {
    owner
        .build_and_publish(
            request(files, sealed_at),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("published generation")
}

pub(super) fn scope() -> CodeIndexGenerationScopeV1 {
    CodeIndexGenerationScopeV1::for_snapshot(&request(&[], 0).snapshot)
}

/// Everything a restored generation answers queries from, independent of
/// its generation identity and lineage.
#[derive(Debug, PartialEq, Eq)]
struct RestoredAnswersV1 {
    chunks: Vec<(String, String)>,
    symbols: Vec<String>,
    edges: Vec<CanonicalRelationEdgeV1>,
    unresolved_calls: Vec<crate::chunks::CodeIndexUnresolvedReferenceV1>,
    imports: Vec<crate::chunks::CodeIndexImportEvidenceV1>,
    statistics: CodeIndexGenerationStatisticsV1,
    coverage: CoverageSummaryV1,
}

fn answers(store: &MemorySealedPublicationStoreV1) -> RestoredAnswersV1 {
    let generation = store
        .decode_active(&scope())
        .expect("active generation decodes")
        .expect("an active generation");
    RestoredAnswersV1 {
        chunks: generation
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| {
                (
                    chunk.id.as_str().to_owned(),
                    chunk.content_digest.as_str().to_owned(),
                )
            })
            .collect(),
        symbols: generation
            .symbols()
            .symbols
            .iter()
            .map(|symbol| symbol.occurrence.as_str().to_owned())
            .collect(),
        edges: generation.edges().to_vec(),
        unresolved_calls: generation.unresolved_calls.clone(),
        imports: generation.imports().to_vec(),
        statistics: generation.generation_statistics().expect("statistics"),
        coverage: *generation.coverage(),
    }
}

fn resolution_index(
    store: &MemorySealedPublicationStoreV1,
    published: &CodeIndexPublishedBuildV1,
) -> super::resolution_index::PartitionedResolutionIndexDescriptorV1 {
    let manifest = store
        .manifest_bytes(&published.manifest().generation_id)
        .expect("sealed manifest");
    super::partitioned_codec::parse_partitioned_manifest(&manifest)
        .expect("manifest parses")
        .resolution_index
}

/// Seal `before` cold, then `after` over it, and require the successor to
/// match a cold seal of `after` in every sealed byte and restored answer.
fn assert_sparse_matches_cold(
    before: &[(&str, &str, &str)],
    after: &[(&str, &str, &str)],
) -> CodeIndexPublishedBuildV1 {
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    let parent = publish(&mut incremental, before, 1_100_000);
    assert_eq!(
        parent.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::NoParent)
    );
    let sparse = publish(&mut incremental, after, 1_200_000);
    assert_eq!(
        sparse.cold_reason(),
        None,
        "an in-place edit seals over its parent"
    );
    assert_eq!(
        store
            .load_active(&scope())
            .expect("active publication")
            .expect("active successor")
            .metadata()
            .expect("verified metadata")
            .manifest()
            .generation_id,
        sparse.manifest().generation_id
    );

    let cold_store = MemorySealedPublicationStoreV1::default();
    let cold = publish(&mut owner(&cold_store), after, 1_200_000);
    assert_eq!(
        cold.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::NoParent)
    );

    assert_eq!(
        sparse.lane_digest(),
        cold.lane_digest(),
        "segments and graph pages"
    );
    assert_eq!(
        sparse.metadata().generation_statistics(),
        cold.metadata().generation_statistics()
    );
    assert_eq!(
        resolution_index(&store, &sparse),
        resolution_index(&cold_store, &cold)
    );
    assert_eq!(answers(&store), answers(&cold_store));
    sparse
}

const UTIL: &str = "pub fn helper() -> u32 { 1 }\npub fn other() -> u32 { 2 }\n";
const MAIN: &str = "pub fn run() -> u32 { crate::util::helper() + crate::util::other() }\n";
const LIB: &str = "pub mod main;\npub mod util;\n";
const SIBLING: &str = "pub fn sibling() -> u32 { crate::util::helper() }\n";

fn rust_tree<'a>(util: &'a str, main: &'a str) -> Vec<(&'a str, &'a str, &'a str)> {
    vec![
        ("src/lib.rs", RUST, LIB),
        ("src/main.rs", RUST, main),
        ("src/sibling.rs", RUST, SIBLING),
        ("src/util.rs", RUST, util),
        ("src/a.rs", RUST, "pub fn a() {}\n"),
        ("src/b.rs", RUST, "pub fn b() {}\n"),
        ("src/c.rs", RUST, "pub fn c() {}\n"),
        ("src/d.rs", RUST, "pub fn d() {}\n"),
        ("src/e.rs", RUST, "pub fn e() {}\n"),
    ]
}

#[test]
fn a_comment_on_a_called_module_reseals_over_the_parent() {
    let edited = format!("{UTIL}// probe line\n");
    let sparse = assert_sparse_matches_cold(&rust_tree(UTIL, MAIN), &rust_tree(&edited, MAIN));
    assert!(
        sparse.projection().request().changes.reused_count > 0,
        "carried chunks are reused"
    );
}

#[test]
fn sparse_file_panic_identifies_the_changed_source_and_preserves_the_parent() {
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    let parent = publish(&mut incremental, &rust_tree(UTIL, MAIN), 1_100_000);
    let edited = format!("{UTIL}// È\n");
    let request = request_capturing(&rust_tree(&edited, MAIN), &["src/util.rs"], 1_200_000);
    let expected_digest = content_digest(edited.as_bytes());
    let error = incremental
        .build_and_publish(request.clone(), &PanicOnFileWorker::new())
        .expect_err("the changed file checkpoint panics");
    assert!(matches!(
        error,
        CodeIndexProductionErrorV1::SourceFilePanicked {
            logical_path,
            content_digest,
            ..
        } if logical_path == "src/util.rs" && content_digest == expected_digest
    ));
    assert_eq!(
        store
            .load_active(&scope())
            .expect("active publication")
            .expect("parent remains active")
            .metadata()
            .expect("verified parent")
            .manifest()
            .generation_id,
        parent.manifest().generation_id
    );
    let successor = incremental
        .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
        .expect("the sparse owner can retry after a file panic");
    assert_eq!(successor.cold_reason(), None);
    assert_ne!(
        successor.manifest().generation_id,
        parent.manifest().generation_id
    );
}

/// Symbols are kept in occurrence order, and a comment re-keys every
/// occurrence of its file, so the edit reorders same-named symbols without
/// changing any of them. Files that only spell those names, and whose calls
/// bind elsewhere, keep their decisions without being decoded.
#[test]
fn a_comment_beside_same_named_symbols_decodes_only_what_it_reaches() {
    let util = (0..8)
        .map(|n| {
            format!("pub struct Kind{n};\nimpl Kind{n} {{ pub fn new() -> Self {{ Kind{n} }} }}\n")
        })
        .collect::<String>()
        + UTIL;
    let edited = format!("{util}// probe line\n");
    let other = "pub fn new() -> u32 { 7 }\n";
    let callers = (0..4)
        .map(|n| {
            (
                format!("src/caller{n}.rs"),
                format!("pub fn caller{n}() {{ let _ = crate::other::new(); }}\n"),
            )
        })
        .collect::<Vec<_>>();
    fn caller_tree<'a>(
        util: &'a str,
        other: &'a str,
        callers: &'a [(String, String)],
    ) -> Vec<(&'a str, &'a str, &'a str)> {
        let mut tree = rust_tree(util, MAIN);
        tree[0].2 = "pub mod consumer;\npub mod util;\npub mod other;\n";
        tree[1].0 = "src/consumer.rs";
        tree.push(("src/other.rs", RUST, other));
        tree.extend(
            callers
                .iter()
                .map(|(path, source)| (path.as_str(), RUST, source.as_str())),
        );
        tree
    }
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    let parent = publish(
        &mut incremental,
        &caller_tree(&util, other, &callers),
        1_100_000,
    );
    let manifest = store
        .manifest_bytes(&parent.manifest().generation_id)
        .expect("parent manifest");
    let manifest =
        super::partitioned_codec::parse_partitioned_manifest(&manifest).expect("manifest parses");
    let snapshot = request(&caller_tree(&util, other, &callers), 0)
        .snapshot
        .files;
    let segment_of = |path: &str| {
        let key = snapshot
            .iter()
            .position(|file| file.logical_path == path)
            .expect("parent file");
        manifest
            .file_segments
            .iter()
            .find(|segment| segment.file_key as usize == key)
            .map(|segment| segment.segment_digest.clone())
            .expect("parent file segment")
    };
    let untouched = callers
        .iter()
        .map(|(path, _)| path.as_str())
        .chain(["src/other.rs"])
        .map(|path| (path, segment_of(path)))
        .collect::<Vec<_>>();
    let reads_before = untouched
        .iter()
        .map(|(_, digest)| store.segment_reads(digest))
        .collect::<Vec<_>>();
    let evidence_of = |path: &str| {
        let key = snapshot
            .iter()
            .position(|file| file.logical_path == path)
            .expect("parent evidence file");
        manifest
            .file_evidence
            .iter()
            .find(|descriptor| descriptor.file_key as usize == key)
            .map(|descriptor| descriptor.segment_digest.clone())
            .expect("a resolved cross-file call has sealed evidence")
    };
    let unrelated_evidence = callers
        .iter()
        .map(|(path, _)| (path.as_str(), evidence_of(path)))
        .collect::<Vec<_>>();
    let evidence_reads_before = unrelated_evidence
        .iter()
        .map(|(_, digest)| store.segment_reads(digest))
        .collect::<Vec<_>>();
    let dependent_evidence = evidence_of("src/consumer.rs");
    let dependent_reads_before = store.segment_reads(&dependent_evidence);

    let sparse = publish(
        &mut incremental,
        &caller_tree(&edited, other, &callers),
        1_200_000,
    );
    assert_eq!(
        sparse.cold_reason(),
        None,
        "a comment seals over its parent"
    );
    for ((path, digest), before) in untouched.iter().zip(reads_before) {
        assert_eq!(
            store.segment_reads(digest),
            before,
            "{path} names `new` but binds nowhere in the edited file, so its segment stays sealed"
        );
    }
    for ((path, digest), before) in unrelated_evidence.iter().zip(evidence_reads_before) {
        assert_eq!(
            store.segment_reads(digest),
            before,
            "{path} has unrelated sealed edges and must not decode its evidence"
        );
    }
    assert!(
        store.segment_reads(&dependent_evidence) > dependent_reads_before,
        "a caller whose sealed edge lands in the edited file must still be repointed"
    );
    assert_sparse_matches_cold(
        &caller_tree(&util, other, &callers),
        &caller_tree(&edited, other, &callers),
    );
}

#[test]
fn a_new_cross_file_call_binds_in_the_successor() {
    let main = "pub fn run() -> u32 { crate::util::helper() + crate::util::other() + crate::sibling::sibling() }\n";
    assert_sparse_matches_cold(&rust_tree(UTIL, MAIN), &rust_tree(UTIL, main));
}

#[test]
fn removing_a_called_function_drops_its_callers_edges() {
    let util = "pub fn other() -> u32 { 2 }\n";
    assert_sparse_matches_cold(&rust_tree(UTIL, MAIN), &rust_tree(util, MAIN));
}

#[test]
fn successive_edits_reseal_explicit_parent_lineage_as_identity() {
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    publish(&mut incremental, &rust_tree(UTIL, MAIN), 1_100_000);
    let first = format!("{UTIL}// first\n");
    assert_eq!(
        publish(&mut incremental, &rust_tree(&first, MAIN), 1_200_000).cold_reason(),
        None
    );
    let main = format!("{MAIN}// second\n");
    let second = publish(&mut incremental, &rust_tree(&first, &main), 1_300_000);
    assert_eq!(second.cold_reason(), None);

    let cold_store = MemorySealedPublicationStoreV1::default();
    let cold = publish(
        &mut owner(&cold_store),
        &rust_tree(&first, &main),
        1_300_000,
    );
    assert_eq!(second.lane_digest(), cold.lane_digest());
    assert_eq!(answers(&store), answers(&cold_store));
    let restored = store
        .decode_active(&scope())
        .expect("decodes")
        .expect("active");
    let util_symbols = restored
        .files
        .iter()
        .find(|file| file.authority.logical_path == "src/util.rs")
        .map(|file| file.artifacts.symbols.len())
        .expect("util file");
    let carried_identity = restored
        .lineage()
        .iter()
        .filter(|candidate| {
            candidate.prior_occurrence == candidate.current_occurrence
                && candidate.kind == crate::lineage::LineageKindV1::Unchanged
        })
        .count();
    assert!(
        carried_identity >= util_symbols,
        "the first edit's file continues unchanged from itself after the second"
    );
}

/// The edit moves only a module's signature, so a caller of `Inner.bar` has
/// no site the edit can move: the module's name moves a lookup only as its
/// last segment. Its sealed edge still targets the edited file's old `bar`
/// occurrence and must be re-pointed at the new one.
#[test]
fn a_module_signature_edit_repoints_calls_through_the_module() {
    let inner = |signature: &str| format!("{signature}\n  def self.bar\n    3\n  end\nend\n");
    fn tree(b: &str) -> Vec<(&str, &str, &str)> {
        vec![
            (
                "lib/a.rb",
                "ruby",
                "require_relative \"b\"\ndef run\n  Inner.bar\nend\n",
            ),
            ("lib/b.rb", "ruby", b),
            ("lib/c.rb", "ruby", "C = 1\n"),
            ("lib/d.rb", "ruby", "D = 1\n"),
            ("lib/e.rb", "ruby", "E = 1\n"),
            ("lib/f.rb", "ruby", "F = 1\n"),
            ("lib/g.rb", "ruby", "G = 1\n"),
            ("lib/h.rb", "ruby", "H = 1\n"),
            ("lib/i.rb", "ruby", "I = 1\n"),
        ]
    }
    let before = inner("module Inner");
    let after = inner("module Inner # moved");
    assert_sparse_matches_cold(&tree(&before), &tree(&after));
}

#[test]
fn typescript_imports_resolve_over_the_parent() {
    let a = "export function helper(): number { return 1; }\n";
    let b = "import { helper } from './a';\nexport function run(): number { return helper(); }\n";
    let tree = |a: &'static str| {
        vec![
            ("web/a.ts", "typescript", a),
            ("web/b.ts", "typescript", b),
            ("web/c.ts", "typescript", "export const c = 1;\n"),
            ("web/d.ts", "typescript", "export const d = 1;\n"),
            ("web/e.ts", "typescript", "export const e = 1;\n"),
            ("web/f.ts", "typescript", "export const f = 1;\n"),
            ("web/g.ts", "typescript", "export const g = 1;\n"),
            ("web/h.ts", "typescript", "export const h = 1;\n"),
            ("web/i.ts", "typescript", "export const i = 1;\n"),
        ]
    };
    assert_sparse_matches_cold(
        &tree(a),
        &tree("export function helper(): number { return 2; }\n"),
    );
}

#[test]
fn python_module_calls_resolve_over_the_parent() {
    let tree = |a: &'static str| {
        vec![
            ("pkg/__init__.py", "python", ""),
            ("pkg/a.py", "python", a),
            (
                "pkg/b.py",
                "python",
                "from pkg.a import helper\n\ndef run():\n    return helper()\n",
            ),
            ("pkg/c.py", "python", "C = 1\n"),
            ("pkg/d.py", "python", "D = 1\n"),
            ("pkg/e.py", "python", "E = 1\n"),
            ("pkg/f.py", "python", "F = 1\n"),
            ("pkg/g.py", "python", "G = 1\n"),
            ("pkg/h.py", "python", "H = 1\n"),
        ]
    };
    assert_sparse_matches_cold(
        &tree("def helper():\n    return 1\n"),
        &tree("def helper():\n    return 2\n"),
    );
}

#[test]
fn import_and_file_set_changes_build_cold() {
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    publish(&mut incremental, &rust_tree(UTIL, MAIN), 1_100_000);
    let main = format!("use crate::util::helper;\n{MAIN}");
    let imported = publish(&mut incremental, &rust_tree(UTIL, &main), 1_200_000);
    assert_eq!(
        imported.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::MovesNameLookups)
    );
    let mut grown = rust_tree(UTIL, &main);
    grown.push(("src/f.rs", RUST, "pub fn f() {}\n"));
    let added = publish(&mut incremental, &grown, 1_300_000);
    assert_eq!(
        added.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::FilesAddedOrRemoved)
    );
    assert_eq!(
        store
            .decode_active(&scope())
            .expect("decode cold publication")
            .expect("cold publication active")
            .statistics,
        *added.metadata().generation_statistics()
    );
}

fn go_tree<'a>(wrong: &'a str, util: &'a str) -> Vec<(&'a str, &'a str, &'a str)> {
    vec![
        ("go.mod", "go", "module example.com/m\n"),
        (
            "calc/adder.go",
            "go",
            "package calc\n\ntype Adder interface {\n\tAdd(a, b int) int\n}\n",
        ),
        ("calc/util.go", "go", util),
        ("calc/wrong.go", "go", wrong),
        ("calc/a.go", "go", "package calc\n\nfunc A() {}\n"),
        ("calc/b.go", "go", "package calc\n\nfunc B() {}\n"),
        ("calc/c.go", "go", "package calc\n\nfunc C() {}\n"),
        ("calc/d.go", "go", "package calc\n\nfunc D() {}\n"),
        ("calc/e.go", "go", "package calc\n\nfunc E() {}\n"),
    ]
}

const GO_WRONG: &str = "package calc\n\ntype Wrong struct{}\n";
const GO_UTIL: &str =
    "package calc\n\nfunc Util() int { return A2() }\n\nfunc A2() int { return 1 }\n";

/// Satisfaction pairs every Go method set with every interface, so an edit
/// that gives a type the interface's method must seal the new `Implements`
/// edge, which no reference site in the edited file names.
#[test]
fn a_go_method_set_edit_builds_cold_and_seals_satisfaction() {
    let wrong =
        "package calc\n\ntype Wrong struct{}\n\nfunc (Wrong) Add(a, b int) int { return a + b }\n";
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    publish(&mut incremental, &go_tree(GO_WRONG, GO_UTIL), 1_100_000);
    let edited = publish(&mut incremental, &go_tree(wrong, GO_UTIL), 1_200_000);
    assert_eq!(
        edited.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::MovesGoMethodSets)
    );
    let cold_store = MemorySealedPublicationStoreV1::default();
    publish(&mut owner(&cold_store), &go_tree(wrong, GO_UTIL), 1_200_000);
    let sealed = answers(&store);
    assert!(
        sealed
            .edges
            .iter()
            .any(|edge| edge.kind == tracedecay_domain::RelationEdgeKindV1::Implements),
        "Wrong implements Adder"
    );
    assert_eq!(sealed, answers(&cold_store));

    let util = format!("{GO_UTIL}// edit\n");
    assert_sparse_matches_cold(&go_tree(GO_WRONG, GO_UTIL), &go_tree(GO_WRONG, &util));
}

#[test]
fn a_large_change_builds_cold() {
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    publish(&mut incremental, &rust_tree(UTIL, MAIN), 1_100_000);
    let util = format!("{UTIL}// edit\n");
    let main = format!("{MAIN}// edit\n");
    let changed = publish(&mut incremental, &rust_tree(&util, &main), 1_200_000);
    assert_eq!(
        changed.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::ChangedShare)
    );
}

fn ambiguous_rust_tree<'a>(main: &'a str, b: &'a str) -> Vec<(&'a str, &'a str, &'a str)> {
    let mut tree = rust_tree(UTIL, main);
    tree[0].2 = "pub mod main; pub mod util; pub mod a;";
    tree[4].2 = "pub fn choice() -> u32 { 1 }";
    tree[5] = ("src/a/mod.rs", RUST, b);
    tree
}

const AMBIGUOUS_CALL: &str = "mod a; pub fn run() -> u32 { crate::a::choice() }";
const NO_AMBIGUOUS_CALL: &str = "mod a; pub fn run() -> u32 { 0 }";

#[test]
fn ambiguity_census_tracks_edited_references() {
    let empty = ambiguous_rust_tree(NO_AMBIGUOUS_CALL, "pub fn choice() -> u32 { 1 }");
    let called = ambiguous_rust_tree(AMBIGUOUS_CALL, "pub fn choice() -> u32 { 1 }");
    let added = assert_sparse_matches_cold(&empty, &called);
    assert_eq!(
        added
            .metadata()
            .generation_statistics()
            .ambiguous_name_drops,
        1
    );
    let removed = assert_sparse_matches_cold(&called, &empty);
    assert_eq!(
        removed
            .metadata()
            .generation_statistics()
            .ambiguous_name_drops,
        0
    );
}

#[test]
fn ambiguity_census_tracks_carried_references() {
    let unique = ambiguous_rust_tree(AMBIGUOUS_CALL, "pub fn different() -> u32 { 2 }");
    let ambiguous = ambiguous_rust_tree(AMBIGUOUS_CALL, "pub fn choice() -> u32 { 1 }");
    let added = assert_sparse_matches_cold(&unique, &ambiguous);
    assert_eq!(
        added
            .metadata()
            .generation_statistics()
            .ambiguous_name_drops,
        1
    );
    let removed = assert_sparse_matches_cold(&ambiguous, &unique);
    assert_eq!(
        removed
            .metadata()
            .generation_statistics()
            .ambiguous_name_drops,
        0
    );
}

#[test]
fn sparse_local_shadowing_preserves_gaps_and_the_generic_ambiguity_census() {
    let clear =
        "import { scale } from './lib';\nexport function clear(): number { return scale(1); }\n";
    let shadowed = "import { scale } from './lib';\ndescribe('scope', () => {\n  function scale(x: number) { return x; }\n  function scale(x: number) { return x + 1; }\n  it('shadowed', () => { scale(1); });\n});\nexport function clear(): number { return scale(1); }\n";
    let tree = |app| {
        vec![
            ("web/app.ts", "typescript", app),
            (
                "web/lib.ts",
                "typescript",
                "export function scale(x: number) { return x * 2; }\n",
            ),
            ("web/a.ts", "typescript", "export const a = 1;\n"),
            ("web/b.ts", "typescript", "export const b = 1;\n"),
            ("web/c.ts", "typescript", "export const c = 1;\n"),
            ("web/d.ts", "typescript", "export const d = 1;\n"),
            ("web/e.ts", "typescript", "export const e = 1;\n"),
            ("web/f.ts", "typescript", "export const f = 1;\n"),
            ("web/g.ts", "typescript", "export const g = 1;\n"),
        ]
    };
    assert_sparse_matches_cold(&tree(clear), &tree(shadowed));
    assert_sparse_matches_cold(&tree(shadowed), &tree(clear));

    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    publish(&mut incremental, &tree(clear), 1_100_000);
    for (app, expected_gaps, sealed_at) in [(shadowed, 1, 1_200_000), (clear, 0, 1_300_000)] {
        let successor = publish(&mut incremental, &tree(app), sealed_at);
        assert_eq!(successor.cold_reason(), None);
        let restored = answers(&store);
        assert_eq!(restored.statistics.ambiguous_name_drops, 0);
        assert_eq!(
            restored
                .unresolved_calls
                .iter()
                .filter(|call| call.ambiguous_local && call.reference_name == "scale")
                .count(),
            expected_gaps
        );
        assert_eq!(
            restored
                .edges
                .iter()
                .filter(|edge| edge.kind == RelationEdgeKindV1::Calls)
                .count(),
            1
        );
    }
}

fn symbol_query(store: &MemorySealedPublicationStoreV1, name: &str) -> Vec<String> {
    let generation = store
        .decode_active(&scope())
        .expect("active generation decodes")
        .expect("an active generation");
    let mut hits = generation
        .symbols()
        .symbols
        .iter()
        .filter(|symbol| symbol.simple_name == name)
        .map(|symbol| symbol.qualified_name.clone())
        .collect::<Vec<_>>();
    hits.sort();
    hits
}

#[test]
fn a_go_method_set_edit_seals_cold_from_changed_bytes_only() {
    let wrong =
        "package calc\n\ntype Wrong struct{}\n\nfunc (Wrong) Add(a, b int) int { return a + b }\n";
    let edited = go_tree(wrong, GO_UTIL);
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    publish(&mut incremental, &go_tree(GO_WRONG, GO_UTIL), 1_100_000);
    let successor = incremental
        .build_and_publish(
            request_capturing(&edited, &["calc/wrong.go"], 1_200_000),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("cold successor from the edited file's bytes");
    assert_eq!(
        successor.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::MovesGoMethodSets)
    );

    let cold_store = MemorySealedPublicationStoreV1::default();
    publish(&mut owner(&cold_store), &edited, 1_200_000);
    assert_eq!(answers(&store), answers(&cold_store));
    for name in ["Add", "Util", "Adder"] {
        assert_eq!(
            symbol_query(&store, name),
            symbol_query(&cold_store, name),
            "spot-checked query {name}"
        );
        assert!(
            !symbol_query(&store, name).is_empty(),
            "spot-checked query {name} must hit"
        );
    }
}

#[test]
fn a_changed_file_without_captured_bytes_is_refused() {
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    publish(&mut incremental, &rust_tree(UTIL, MAIN), 1_100_000);
    let util = format!("{UTIL}// edit\n");
    let error = incremental
        .build_and_publish(
            request_capturing(&rust_tree(&util, MAIN), &[], 1_200_000),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect_err("changed bytes that were never captured");
    assert!(matches!(
        error,
        CodeIndexProductionErrorV1::Input(CodeIndexInputErrorV1::MissingCapturedFile)
    ));
}

#[test]
fn a_language_changed_file_without_captured_bytes_is_refused() {
    let store = MemorySealedPublicationStoreV1::default();
    let mut incremental = owner(&store);
    let mut before = go_tree(GO_WRONG, GO_UTIL);
    before.push(("config.h", "c", "// shared header\n"));
    publish(&mut incremental, &before, 1_100_000);

    let wrong =
        "package calc\n\ntype Wrong struct{}\n\nfunc (Wrong) Add(a, b int) int { return a + b }\n";
    let mut edited = go_tree(wrong, GO_UTIL);
    edited.push(("config.h", "cpp", "// shared header\n"));
    let error = match incremental.build_and_publish(
        request_capturing(&edited, &["calc/wrong.go"], 1_200_000),
        &UninterruptibleCodeIndexControlV1,
    ) {
        Ok(_) => panic!("a language change requires captured bytes despite equal content"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CodeIndexProductionErrorV1::Input(CodeIndexInputErrorV1::MissingCapturedFile)
    ));
}

#[test]
fn sparse_lookup_window_shares_canonical_page_and_releases_it() {
    let store = MemorySealedPublicationStoreV1::default();
    let mut owner = owner(&store);
    let request = request(
        &[(
            "src/lib.rs",
            "rust",
            "pub fn alpha() {}\npub fn beta() {}\n",
        )],
        1_000_000,
    );
    let memory = Arc::clone(&request.resident_memory);
    let published = owner
        .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
        .expect("publish canonical lookup fixture");
    let sealed = published.sealed_generation();
    let canonical = sealed.segment_reader();
    let reads = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&reads);
    let failure = Arc::clone(&fail);
    let reader: Arc<SealedSegmentReaderV1> = Arc::new(move |segment, bytes| {
        observed.fetch_add(1, Ordering::Relaxed);
        if failure.load(Ordering::Relaxed) {
            return Err(CodeIndexProductionErrorV1::Contract(
                "reader failure".to_owned(),
            ));
        }
        canonical(segment, bytes)
    });
    let parent = SealedParentGenerationV1::open(CodeIndexSealedGenerationV1::new(
        Arc::from(sealed.manifest_bytes()),
        reader,
    ))
    .expect("open canonical parent");
    let mut present = parent
        .snapshot()
        .files
        .iter()
        .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
        .collect::<Vec<_>>();
    present.sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
    let paths = present
        .iter()
        .enumerate()
        .map(|(index, file)| (file.logical_path.as_str(), index))
        .collect::<HashMap<_, _>>();
    let index = parent
        .resolution_index()
        .expect("canonical resolution index");
    let operation = memory.begin_operation();
    let window = SparseDefinitionWindowV1::new(&index, &operation).expect("lookup window");
    let before =
        SparseSymbolsByNameV1::new(&index, &paths, &window, std::iter::empty(), &operation)
            .expect("before overlay");
    let after = SparseSymbolsByNameV1::new(&index, &paths, &window, std::iter::empty(), &operation)
        .expect("after overlay");
    assert!(before.get("alpha").expect("first name").is_some());
    assert!(after.get("beta").expect("second name").is_some());
    assert_eq!(
        reads.load(Ordering::Relaxed),
        1,
        "before and after share one canonical page"
    );
    window.release().expect("release first window");
    assert!(after.get("alpha").expect("next window").is_some());
    assert_eq!(
        reads.load(Ordering::Relaxed),
        2,
        "next window reads the canonical page again"
    );
    window.release().expect("release second window");
    fail.store(true, Ordering::Relaxed);
    assert!(
        matches!(after.get("beta"), Err(CodeIndexProductionErrorV1::Contract(message)) if message == "reader failure")
    );
    assert_eq!(
        reads.load(Ordering::Relaxed),
        3,
        "reader failure reaches the canonical loader"
    );
}

#[test]
fn ruby_require_memo_releases_its_charged_workspace_and_retries_errors() {
    let store = MemorySealedPublicationStoreV1::default();
    let mut owner = owner(&store);
    publish(
        &mut owner,
        &[
            (
                "lib/a.rb",
                "ruby",
                "require_relative 'b'\ndef run\n  Inner.bar\nend\n",
            ),
            (
                "lib/b.rb",
                "ruby",
                "require_relative 'c'\nmodule Inner\n  def self.bar\n    3\n  end\nend\n",
            ),
            ("lib/c.rb", "ruby", "require_relative 'a'\nC = 1\n"),
        ],
        1_000_000,
    );
    let decoded = store.decode_active(&scope()).unwrap().unwrap();
    let files = &decoded.files;
    let index = files
        .iter()
        .position(|file| file.authority.logical_path == "lib/a.rb")
        .unwrap();
    let reference = files[index]
        .artifacts
        .unresolved_references
        .iter()
        .find(|reference| reference.reference_name == "Inner.bar")
        .unwrap();
    let mut modules = ModuleImportIndexV1::new(files).unwrap();
    let refuse = || CodeIndexProductionErrorV1::Contract("closure admission refused".to_owned());
    assert!(
        matches!(modules.call_outcome(index, reference, &mut |_| Err(refuse())),
        Err(CodeIndexProductionErrorV1::Contract(message)) if message == "closure admission refused")
    );
    let mut charged = 0;
    let first = modules
        .call_outcome(index, reference, &mut |bytes| {
            charged = bytes.apply_to(charged)?;
            Ok(())
        })
        .unwrap();
    let Some(ImportBindingOutcomeV1::Bound(target, symbol)) = first else {
        panic!("the canonical require cycle binds Inner.bar");
    };
    assert!(
        charged > 0,
        "the closure must be admitted before allocation"
    );
    assert!(
        matches!(modules.call_outcome(index, reference, &mut |_| Err(refuse())).unwrap(),
        Some(ImportBindingOutcomeV1::Bound(index, ref row)) if index == target && row.occurrence == symbol.occurrence)
    );
    modules.release_file_memo(index);
    assert!(
        matches!(modules.call_outcome(index, reference, &mut |_| Err(refuse())),
        Err(CodeIndexProductionErrorV1::Contract(message)) if message == "closure admission refused")
    );
    assert!(
        matches!(modules.call_outcome(index, reference, &mut |_| Ok(())).unwrap(),
        Some(ImportBindingOutcomeV1::Bound(index, ref row)) if index == target && row.occurrence == symbol.occurrence)
    );
}

#[test]
fn carried_evidence_visitors_release_each_files_scratch_before_the_next_file() {
    let mut sources = (0..24).map(|index| (
        format!("lib/caller_{index}.rb"),
        "ruby",
        format!("require_relative 'target'\nrequire_relative 'missing'\ndef run_{index}\n  Target.helper\n  Missing.call\nend\n"),
    )).collect::<Vec<_>>();
    sources.push((
        "lib/target.rb".to_owned(),
        "ruby",
        "module Target\n  def self.helper\n    1\n  end\nend\n".to_owned(),
    ));
    let rows = sources
        .iter()
        .map(|(path, language, source)| (path.as_str(), *language, source.as_str()))
        .collect::<Vec<_>>();
    let store = MemorySealedPublicationStoreV1::default();
    let mut owner = owner(&store);
    let request = request(&rows, 1_000_000);
    let fixture_memory = Arc::clone(&request.resident_memory);
    let published = owner
        .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
        .unwrap();
    let parent = SealedParentGenerationV1::open(published.sealed_generation().clone()).unwrap();
    let decoded = store.decode_active(&scope()).unwrap().unwrap();
    let fixture_operation = fixture_memory.begin_operation();
    let baseline = fixture_memory.snapshot().used_bytes;
    let mut one_file_peak = 0;
    for descriptor in &parent.generation().file_segments {
        let file = &parent.snapshot().files[descriptor.file_key as usize];
        let facts = decoded
            .files
            .iter()
            .find(|facts| facts.authority.logical_path == file.logical_path)
            .unwrap();
        let Some(evidence) = parent.file_evidence(descriptor.file_key).unwrap() else {
            continue;
        };
        evidence
            .for_each_sealed_edge(facts.as_ref(), &fixture_operation, &mut |_, _, _, _, _| {
                one_file_peak = one_file_peak.max(fixture_memory.snapshot().used_bytes - baseline);
                Ok(())
            })
            .unwrap();
        evidence
            .for_each_unresolved_call(facts.as_ref(), &fixture_operation, &mut |_| {
                one_file_peak = one_file_peak.max(fixture_memory.snapshot().used_bytes - baseline);
                Ok(())
            })
            .unwrap();
        assert_eq!(fixture_memory.snapshot().used_bytes, baseline);
    }
    let limit = NonZeroU64::new(one_file_peak).expect("real fixture has evidence scratch");
    let memory = Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::new(ResidentMemoryPressureV1::with_sampler(
            limit,
            Arc::new(|| {
                Some(ProcessResidentSampleV1 {
                    resident_bytes: 0,
                    unreclaimable_bytes: 0,
                    swapped_bytes: 0,
                    cgroup_headroom_bytes: None,
                })
            }),
        )),
    ));
    let operation = memory.begin_operation();
    let mut visited_edges = 0;
    let mut visited_calls = 0;
    for descriptor in &parent.generation().file_segments {
        let file = &parent.snapshot().files[descriptor.file_key as usize];
        let facts = decoded
            .files
            .iter()
            .find(|facts| facts.authority.logical_path == file.logical_path)
            .unwrap();
        let Some(evidence) = parent.file_evidence(descriptor.file_key).unwrap() else {
            continue;
        };
        evidence
            .for_each_sealed_edge(facts.as_ref(), &operation, &mut |_, _, _, _, _| {
                visited_edges += 1;
                assert!(memory.snapshot().used_bytes <= one_file_peak);
                Ok(())
            })
            .unwrap();
        assert_eq!(memory.snapshot().used_bytes, 0);
        evidence
            .for_each_unresolved_call(facts.as_ref(), &operation, &mut |_| {
                visited_calls += 1;
                assert!(memory.snapshot().used_bytes <= one_file_peak);
                Ok(())
            })
            .unwrap();
        assert_eq!(memory.snapshot().used_bytes, 0);
    }
    assert!(visited_edges >= 24);
    assert!(visited_calls >= 24);
}
