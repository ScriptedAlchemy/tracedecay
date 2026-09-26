//! `CodeIndexPublishedGenerationV1::retained_bytes` is what admission charges
//! for decoding a sealed generation and what the resident-memory inventory
//! reports for the decode it holds. This binary counts every allocation, so
//! the bytes a real decode leaves live are the reference it is held to, and
//! the bytes a sealed generation's graph build holds are bounded against it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::graph_projection::{
    CODE_GRAPH_PROJECTOR_REVISION, build_sealed_code_graph_rows, code_graph_projection_identity,
};
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1,
    CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1,
    CodeIndexPublishedGenerationV1, CodeIndexRepositoryParseIdentityV1,
    SealedGenerationFileWindowsV1, SealedGenerationSegmentPublicationV1,
    SealedGenerationSegmentReadV1,
};
use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
    ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
};
use tracedecay_domain::test_fixtures::id;
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, FileOccurrenceId, LanguageId, ManifestDigest,
    PolicyRevisionId, PrivacyDomainId, ProjectId, ProjectionBatchRequestV1, ProjectionKeyV1,
    ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1, RepositoryDirtyStateV1,
    RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1, UtcMicros,
};
use tracedecay_graph_db::{GraphGenerationRowSpill, GraphNamespace, GraphProjectorRevision};

struct CountingAllocator;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(live, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if size >= layout.size() {
            let live =
                LIVE.fetch_add(size - layout.size(), Ordering::Relaxed) + size - layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// The counters are process-wide, so measurements run one at a time.
static MEASUREMENT: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct Publication;

impl CodeIndexAtomicPublicationPort for Publication {
    fn load_active(
        &self,
        _scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(None)
    }

    fn publish_atomically(
        &mut self,
        _scope: &CodeIndexGenerationScopeV1,
        _expected_active_generation: Option<&CodeGenerationId>,
        _generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        Ok(())
    }
}

struct Projection;

impl CodeChunkProjectionSink for Projection {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let decisions = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: change.current_digest.clone(),
            })
            .collect::<Vec<_>>();
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

struct Active;

impl CodeIndexExecutionControlV1 for Active {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

/// A Rust module that exercises every per-file artifact: documented public
/// items, a struct with methods, imports of sibling modules, cross-module
/// calls the seal binds, and bare calls it leaves unresolved.
fn module_source(ordinal: usize) -> String {
    let next = (ordinal + 1) % 300;
    format!(
        "//! Module {ordinal}.\n\nuse crate::module_{next:03}::transform_{next};\nuse std::collections::HashMap;\n\n\
         /// Transform the input for module {ordinal}.\npub fn transform_{ordinal}(input: &str, limit: usize) -> usize {{\n    let trimmed = input.trim();\n    let mut total = 0;\n    for (index, part) in trimmed.split(',').enumerate() {{\n        if index >= limit {{ break; }}\n        total += part.len() * {ordinal};\n    }}\n    total + helper_value(total)\n}}\n\n\
         pub fn describe_{ordinal}(value: u64) -> String {{\n    let doubled = value * 2;\n    let label = format!(\"{{doubled}}-{ordinal}\");\n    transform_{next}(&label, 3);\n    label.to_uppercase()\n}}\n\n\
         pub struct Holder{ordinal} {{\n    map: HashMap<u32, u32>,\n}}\n\n\
         impl Holder{ordinal} {{\n    pub fn get(&self, key: u32) -> Option<u32> {{\n        self.map.get(&key).copied()\n    }}\n\n    pub fn total(&self) -> u32 {{\n        self.map.values().sum::<u32>() + external_sum(self.map.len())\n    }}\n}}\n"
    )
}

fn request(files: usize) -> CodeIndexBuildRequestV1 {
    let mut snapshot_files = Vec::new();
    let mut captured = Vec::new();
    let mut receipts = Vec::new();
    let mut identity = Sha256::new();
    for ordinal in 0..files {
        let source = module_source(ordinal);
        let path = format!("src/module_{ordinal:03}.rs");
        identity.update(path.as_bytes());
        identity.update([0]);
        identity.update(source.as_bytes());
        let occurrence = id::<FileOccurrenceId>(&format!("file.accounting.{ordinal:03}"));
        snapshot_files.push(SanitizedCodeFileV1 {
            file_occurrence_id: occurrence.clone(),
            logical_path: path,
            language: Some(id::<LanguageId>("rust")),
            content_digest: content_digest(source.as_bytes()),
            disposition: SnapshotFileDispositionV1::Present,
        });
        captured.push(CodeIndexCapturedFileV1 {
            file_occurrence_id: occurrence,
            sanitized_bytes: Arc::from(source.as_bytes()),
            sensitivity_level: SensitivityLevelV1::Public,
        });
        receipts.push(id::<SanitizationReceiptId>(&format!(
            "receipt.accounting.{ordinal:03}"
        )));
    }
    CodeIndexBuildRequestV1 {
        snapshot: SanitizedCodeSnapshotV1 {
            repository: id::<RepositoryId>("repository.accounting"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
            sanitization_receipts: receipts,
            content_identity: content_digest(&identity.finalize()),
            captured_at: UtcMicros(1_000_000),
            files: snapshot_files,
        },
        captured_files: captured,
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
        },
        sealed_at: UtcMicros(2_000_000),
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.v1".to_owned(),
            profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "e".repeat(64))),
        },
    }
}

fn config() -> CodeIndexProductionConfigV1 {
    CodeIndexProductionConfigV1 {
        project_id: id::<ProjectId>("project.accounting"),
        repository: id::<RepositoryId>("repository.accounting"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        policy_revision: id::<PolicyRevisionId>("policy.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v2"),
        privacy_domain: id::<PrivacyDomainId>("privacy.accounting"),
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    }
}

fn seal(generation: &CodeIndexPublishedGenerationV1) -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let mut segments = BTreeMap::new();
    let mut evidence = Vec::new();
    let manifest = generation
        .encode_partitioned_sealed(|publication| {
            match publication {
                SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                    segments.insert(digest.as_str().to_owned(), bytes.to_vec());
                }
                SealedGenerationSegmentPublicationV1::GenerationEvidencePage { bytes, .. } => {
                    evidence.extend_from_slice(bytes);
                }
                SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                    segment_digest,
                    ..
                } => {
                    segments.insert(
                        segment_digest.as_str().to_owned(),
                        std::mem::take(&mut evidence),
                    );
                }
            }
            Ok(())
        })
        .expect("seal");
    (manifest, segments)
}

fn read_segment(
    segments: &BTreeMap<String, Vec<u8>>,
    request: SealedGenerationSegmentReadV1<'_>,
    buffer: &mut Vec<u8>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let (digest, offset, length) = match request {
        SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => (digest, 0, size_bytes),
        SealedGenerationSegmentReadV1::Range {
            digest,
            offset,
            length,
            ..
        } => (digest, offset, length),
    };
    let bytes = segments
        .get(digest.as_str())
        .ok_or_else(|| CodeIndexProductionErrorV1::Contract("segment missing".to_owned()))?;
    let start = usize::try_from(offset).expect("offset");
    let end = start + usize::try_from(length).expect("length");
    buffer.clear();
    buffer.extend_from_slice(&bytes[start..end]);
    Ok(())
}

fn decode(manifest: &[u8], segments: &BTreeMap<String, Vec<u8>>) -> CodeIndexPublishedGenerationV1 {
    CodeIndexPublishedGenerationV1::decode_partitioned_sealed(manifest, |request, buffer| {
        read_segment(segments, request, buffer)
    })
    .expect("decode")
}

/// Publishing a sealed generation's code graph reads its segments one window
/// at a time: the most the build ever holds above the sealed input stays
/// within a fixed budget, below the 22.3 MB decoding this generation alone
/// leaves live. Decoding the whole generation and projecting it in one piece
/// peaked at 38,827,295 bytes for the same 4,201 entities and 5,100 relations.
#[test]
fn a_sealed_graph_build_holds_windows_not_the_decoded_generation() {
    const PEAK_BUDGET_BYTES: usize = 19_000_000;
    let _measurement = MEASUREMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let built = CodeIndexProductionOwnerV1::new(config(), Publication, Projection)
        .expect("owner")
        .build_and_publish(request(300), &Active)
        .expect("build");
    let (manifest, segments) = seal(&built);
    drop(built);
    let scratch = tempfile::tempdir().expect("scratch");
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph-resident").expect("ns"))
            .expect("projection");
    let revision =
        GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned()).expect("rev");
    let spill = GraphGenerationRowSpill::create(scratch.path().join("rows"), projection.clone())
        .expect("spill");

    // One worker reads four files per window, so the 300-file fixture spans
    // 75 windows the way a production corpus spans its windows.
    tracedecay_code_index::parallelism::force_indexing_workers_for_test(1);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let source = SealedGenerationFileWindowsV1::open(&manifest).expect("sealed manifest");
    let spilled = build_sealed_code_graph_rows(
        projection,
        &source,
        &mut |request, buffer| read_segment(&segments, request, buffer),
        &revision,
        spill,
        &|| Ok(()),
    )
    .expect("sealed graph builds");
    let peak = PEAK.load(Ordering::Relaxed) - before;
    tracedecay_code_index::parallelism::clear_forced_indexing_workers_for_test();
    eprintln!("GRAPH ROWS peak {peak}");

    assert_eq!(spilled.row_counts(), (4_201, 5_100));
    assert!(
        peak <= PEAK_BUDGET_BYTES,
        "the graph build held {peak} bytes at peak, over its {PEAK_BUDGET_BYTES}-byte budget"
    );
}

#[test]
fn retained_bytes_account_for_what_a_decode_leaves_live() {
    let _measurement = MEASUREMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let built = CodeIndexProductionOwnerV1::new(config(), Publication, Projection)
        .expect("owner")
        .build_and_publish(request(300), &Active)
        .expect("build");
    let (manifest, segments) = seal(&built);
    drop(built);

    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let decoded = decode(&manifest, &segments);
    let live = LIVE.load(Ordering::Relaxed) - before;
    eprintln!("ACCOUNTING peak {}", PEAK.load(Ordering::Relaxed) - before);
    let retained = usize::try_from(decoded.retained_bytes()).expect("retained");
    eprintln!("ACCOUNTING live {live} retained {retained}");
    assert!(
        retained * 10 >= live * 9 && retained * 10 <= live * 11,
        "a decode left {live} bytes live but retained_bytes() reports {retained}"
    );
}
