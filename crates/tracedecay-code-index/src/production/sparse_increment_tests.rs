//! A successor sealed over its parent must seal what a cold build of the
//! same tree seals: the same file segments and graph pages, the same
//! statistics, coverage, and resolution index, and the same restored edges
//! and call limitations.

use sha2::Digest as _;
use tracedecay_domain::{LanguageId, ProjectionKeyV1, ProjectionKindV1, SanitizationReceiptId};

use super::worker_tests::{WorkerProjectionSink, worker_config, worker_id};
use super::*;

const RUST: &str = "rust";

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

fn request(files: &[(&str, &str, &str)], sealed_at: i64) -> CodeIndexBuildRequestV1 {
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

type Owner = CodeIndexProductionOwnerV1<MemorySealedPublicationStoreV1, WorkerProjectionSink>;

fn owner(store: &MemorySealedPublicationStoreV1) -> Owner {
    CodeIndexProductionOwnerV1::new(worker_config(), store.clone(), WorkerProjectionSink)
        .expect("production owner")
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

fn scope() -> CodeIndexGenerationScopeV1 {
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
    assert!(
        sparse.decoded().is_none(),
        "a sparse successor holds no decoded generation"
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
    assert!(
        added.decoded().is_some(),
        "a cold build holds its decoded generation"
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
        Some(1)
    );
    let removed = assert_sparse_matches_cold(&called, &empty);
    assert_eq!(
        removed
            .metadata()
            .generation_statistics()
            .ambiguous_name_drops,
        Some(0)
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
        Some(1)
    );
    let removed = assert_sparse_matches_cold(&ambiguous, &unique);
    assert_eq!(
        removed
            .metadata()
            .generation_statistics()
            .ambiguous_name_drops,
        Some(0)
    );
}

#[test]
fn ambiguity_census_preserves_unknown_parent_count() {
    let mut store = MemorySealedPublicationStoreV1::default();
    let before = ambiguous_rust_tree(AMBIGUOUS_CALL, "pub fn choice() -> u32 { 1 }");
    let initial = publish(&mut owner(&store), &before, 1_100_000);
    let mut legacy = store
        .decode_active(&scope())
        .expect("decode")
        .expect("active");
    assert_eq!(legacy.statistics.ambiguous_name_drops, Some(1));
    let mut statistics = serde_json::to_value(&legacy.statistics).expect("statistics encode");
    statistics
        .as_object_mut()
        .expect("statistics object")
        .remove("ambiguous_name_drops");
    legacy.statistics = serde_json::from_value(statistics).expect("legacy statistics decode");
    store
        .publish_atomically(
            &scope(),
            Some(&initial.manifest().generation_id),
            &CodeIndexSealedPublicationV1::Cold(
                Arc::new(legacy),
                CodeIndexColdBuildReasonV1::NoParent,
            ),
        )
        .expect("legacy publication");
    let after = ambiguous_rust_tree(NO_AMBIGUOUS_CALL, "pub fn choice() -> u32 { 1 }");
    let sparse = publish(&mut owner(&store), &after, 1_200_000);
    assert_eq!(sparse.cold_reason(), None);
    assert_eq!(
        sparse
            .metadata()
            .generation_statistics()
            .ambiguous_name_drops,
        None
    );
    assert_eq!(answers(&store).statistics.ambiguous_name_drops, None);
}
