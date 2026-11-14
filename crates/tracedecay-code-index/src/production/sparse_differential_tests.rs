//! Every in-place edit of the cross-file fixtures, sealed over its parent,
//! must seal what a cold build of the edited tree seals.

use std::path::Path;

use tracedecay_domain::{EdgeAuthorityV1, LanguageId, SanitizationReceiptId, SensitivityLevelV1};

use super::helpers::{edge_evidence, resolve_cross_file_references_charged};
use super::worker_tests::{
    WorkerPublicationStore, worker_config, worker_id, worker_request_with_source,
};
use super::*;

pub(super) const FIXTURE_ROOT: &str = "../tracedecay-code-extraction/fixtures/cross-file-calls";

/// Every file under `root`, path-sorted, as `(root-relative path, source)`.
pub(super) fn fixture_files(root: &Path) -> Vec<(String, String)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let mut entries = std::fs::read_dir(dir)
            .expect("fixture directory")
            .map(|entry| entry.expect("fixture entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("fixture path under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((
                    relative,
                    std::fs::read_to_string(&path).expect("fixture source"),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files
}

pub(super) fn language_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("py") => "python",
        Some("go" | "mod") => "go",
        Some("java") => "java",
        Some("rb") => "ruby",
        Some("rs") => "rust",
        Some("toml") => "toml",
        Some("ts") => "typescript",
        Some("json") => "json",
        other => panic!("fixture file {path} has an unexpected extension {other:?}"),
    }
}

/// Publishes a generation of `files` through `owner`. File identities
/// follow path order, so two trees with the same paths share them, except
/// that the `fresh` file gets its own occurrence as an edit does. Over the
/// owner's active generation only the fresh file's bytes are captured, as a
/// watched refresh captures only changed files. A cold successor carries
/// unchanged files from the sealed parent instead of recapturing them.
pub(super) fn publish<P: CodeIndexAtomicPublicationPort>(
    owner: &mut CodeIndexProductionOwnerV1<P>,
    files: &[(String, String)],
    fresh: Option<usize>,
    over_parent: bool,
    sealed_at: i64,
) -> (CodeIndexPublishedBuildV1, ResidentMemoryOperationV1) {
    let request = fixture_request(files, fresh, over_parent, sealed_at);
    let operation = request.resident_memory.begin_operation();
    let published = owner
        .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
        .expect("publish the fixture tree");
    (published, operation)
}

fn fixture_request(
    files: &[(String, String)],
    fresh: Option<usize>,
    over_parent: bool,
    sealed_at: i64,
) -> CodeIndexBuildRequestV1 {
    let mut request = worker_request_with_source("file.changed.seed", sealed_at, b"");
    request.snapshot.files.clear();
    request.snapshot.sanitization_receipts.clear();
    request.captured_files.clear();
    let mut identity = Vec::new();
    for (ordinal, (path, source)) in files.iter().enumerate() {
        let edit = if fresh == Some(ordinal) {
            format!(".edited.{sealed_at}")
        } else {
            String::new()
        };
        let file_occurrence_id =
            worker_id::<FileOccurrenceId>(&format!("file.changed.{ordinal:03}{edit}"));
        let bytes = source.as_bytes();
        request.snapshot.files.push(SanitizedCodeFileV1 {
            file_occurrence_id: file_occurrence_id.clone(),
            logical_path: path.clone(),
            language: Some(worker_id::<LanguageId>(language_for(path))),
            content_digest: content_digest(bytes),
            disposition: SnapshotFileDispositionV1::Present,
        });
        request
            .snapshot
            .sanitization_receipts
            .push(worker_id::<SanitizationReceiptId>(&format!(
                "receipt.changed.{ordinal:03}"
            )));
        if !over_parent || fresh == Some(ordinal) {
            request.captured_files.push(CodeIndexCapturedFileV1 {
                file_occurrence_id,
                sanitized_bytes: Arc::from(bytes),
                sensitivity_level: SensitivityLevelV1::Public,
            });
        }
        identity.extend_from_slice(path.as_bytes());
        identity.push(0);
        identity.extend_from_slice(bytes);
    }
    if let Some(fresh) = fresh {
        request.changed_files.insert(files[fresh].0.clone());
    }
    request.snapshot.content_identity = content_digest(&identity);
    request
}

/// `source` with every whole-identifier `name` renamed to `renamed`.
fn rename_identifier(source: &str, name: &str, renamed: &str) -> String {
    let is_identifier = |character: char| character.is_alphanumeric() || character == '_';
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + name.len()..].chars().next();
        out.push_str(&rest[..at]);
        if before.is_some_and(is_identifier) || after.is_some_and(is_identifier) {
            out.push_str(name);
        } else {
            out.push_str(renamed);
        }
        rest = &rest[at + name.len()..];
    }
    out.push_str(rest);
    out
}

#[derive(Debug, Default, PartialEq, Eq)]
struct DifferentialCounts {
    /// Edits sealed over their parent.
    sparse: usize,
    /// Edits whose edited file moved name lookups or Go method sets, sealed
    /// cold.
    cold: usize,
    /// Sparse edits whose cross-file edges or call limitations moved.
    moved: usize,
}

/// What a restored generation answers queries from, independent of its
/// generation identity and lineage.
fn restored_answers(
    store: &WorkerPublicationStore,
    published: &CodeIndexPublishedBuildV1,
    operation: &ResidentMemoryOperationV1,
) -> (
    Vec<CanonicalRelationEdgeV1>,
    Vec<crate::chunks::CodeIndexUnresolvedReferenceV1>,
) {
    let restored = store
        .decode_active(
            &CodeIndexGenerationScopeV1::for_snapshot(published.snapshot()),
            operation,
        )
        .expect("the published generation restores")
        .expect("an active generation");
    (restored.edges, restored.unresolved_calls)
}

/// Publishes `base_tree` and then `edited_tree`, which differ only in file
/// `index`, through one owner, so the edit builds over its sealed parent.
/// Asserts that the edit seals the file segments and graph pages, the
/// statistics, and the restored edges and call limitations a cold build of
/// the edited tree seals, and that only an edit that moved name lookups or
/// Go method sets resolved the corpus whole.
fn assert_seals_like_cold(
    base_tree: &[(String, String)],
    base_fresh: Option<usize>,
    edited_tree: &[(String, String)],
    index: usize,
    counts: &mut DifferentialCounts,
) {
    let store = WorkerPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), store.clone()).expect("production owner");
    let (parent, parent_operation) = publish(&mut owner, base_tree, base_fresh, false, 1_100_000);
    let parent_answers = restored_answers(&store, &parent, &parent_operation);
    super::helpers::take_seal_reference_resolutions();
    let (edited, edited_operation) = publish(&mut owner, edited_tree, Some(index), true, 1_200_000);
    let build_resolutions = super::helpers::take_seal_reference_resolutions();
    let path = &edited_tree[index].0;

    let cold_store = WorkerPublicationStore::default();
    let (cold, cold_operation) = publish(
        &mut CodeIndexProductionOwnerV1::new(worker_config(), cold_store.clone())
            .expect("cold production owner"),
        edited_tree,
        Some(index),
        false,
        1_200_000,
    );
    super::helpers::take_seal_reference_resolutions();
    let answers = restored_answers(&store, &edited, &edited_operation);
    assert_eq!(
        answers,
        restored_answers(&cold_store, &cold, &cold_operation),
        "restored answers after editing {path}"
    );
    assert_eq!(
        edited.metadata().generation_statistics(),
        cold.metadata().generation_statistics(),
        "statistics after editing {path}"
    );
    match edited.cold_reason() {
        None => {
            assert_eq!(
                edited.lane_digest(),
                cold.lane_digest(),
                "segments and pages after editing {path}"
            );
            assert_eq!(
                build_resolutions, 0,
                "the build over the parent after editing {path}"
            );
            counts.sparse += 1;
            if answers != parent_answers {
                counts.moved += 1;
            }
        }
        Some(
            CodeIndexColdBuildReasonV1::MovesNameLookups
            | CodeIndexColdBuildReasonV1::MovesGoMethodSets,
        ) => {
            assert_eq!(build_resolutions, 1, "the whole build after editing {path}");
            counts.cold += 1;
        }
        Some(reason) => panic!("editing {path} built cold for {}", reason.as_str()),
    }
}

/// Every in-place edit of every source file of one language's fixture: a
/// shift that moves every symbol occurrence of the file, and each declared
/// name renamed away and back, so unchanged callers lose and regain their
/// targets.
fn differential_counts(language: &str) -> DifferentialCounts {
    let tree = fixture_files(&Path::new(FIXTURE_ROOT).join(language));
    let store = WorkerPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), store.clone()).expect("production owner");
    let (base, operation) = publish(&mut owner, &tree, None, false, 1_000_000);
    let base = store
        .decode_active(
            &CodeIndexGenerationScopeV1::for_snapshot(base.snapshot()),
            &operation,
        )
        .expect("decode cold fixture")
        .expect("cold fixture active");
    let mut counts = DifferentialCounts::default();
    for index in 0..tree.len() {
        let (path, source) = &tree[index];
        if matches!(language_for(path), "toml" | "json") || path.ends_with("go.mod") {
            continue;
        }
        let mut shifted = tree.clone();
        shifted[index].1 = format!("\n\n{source}");
        assert_seals_like_cold(&tree, None, &shifted, index, &mut counts);

        let file = base
            .files
            .iter()
            .find(|file| &file.authority.logical_path == path)
            .expect("fixture file sealed");
        let mut names = file
            .artifacts
            .symbols
            .iter()
            .map(|symbol| symbol.simple_name.clone())
            .filter(|name| {
                name.chars()
                    .all(|character| character.is_alphanumeric() || character == '_')
            })
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        for name in names {
            let mut renamed = tree.clone();
            renamed[index].1 = rename_identifier(source, &name, &format!("{name}Renamed"));
            assert_seals_like_cold(&tree, None, &renamed, index, &mut counts);
            assert_seals_like_cold(&renamed, Some(index), &tree, index, &mut counts);
        }
    }
    counts
}

/// The actual cold publisher must account for every accepted resolution
/// index segment, in addition to completed fact, binding, evidence and graph
/// page work. Decoding the published manifest supplies the physical page count
/// rather than a test-authored approximation.
#[test]
fn cold_resolution_index_publication_advances_actual_work_counts() {
    #[derive(Default)]
    struct ResolutionWork(std::sync::Mutex<Vec<u64>>, std::sync::atomic::AtomicUsize);

    impl CodeIndexExecutionControlV1 for ResolutionWork {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }

        fn observed_preseal_work(&self, work: CodeIndexPresealWorkV1) {
            assert_eq!(
                self.1.load(std::sync::atomic::Ordering::Acquire),
                0,
                "no pre-seal work may arrive after the durable handoff"
            );
            if work.phase == CodeIndexPresealWorkPhaseV1::Resolution {
                self.0
                    .lock()
                    .expect("resolution observations")
                    .push(work.completed_items);
            }
        }

        fn finish_preseal_observation(&self) {
            assert_eq!(
                self.1.fetch_add(1, std::sync::atomic::Ordering::AcqRel),
                0,
                "publication must finish its pre-seal attempt exactly once"
            );
        }
    }

    let files = vec![
        (
            "src/alpha.rs".to_owned(),
            "pub fn alpha() { crate::beta::beta(); }\n".to_owned(),
        ),
        ("src/beta.rs".to_owned(), "pub fn beta() {}\n".to_owned()),
    ];
    let store = WorkerPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), store.clone()).expect("production owner");
    let observer = ResolutionWork::default();
    let request = fixture_request(&files, None, false, 1_500_000);
    let operation = request.resident_memory.begin_operation();
    let published = owner
        .build_and_publish(request, &observer)
        .expect("real cold reference resolution");
    assert_eq!(
        observer.1.load(std::sync::atomic::Ordering::Acquire),
        1,
        "cold publication must retire pre-seal before the control is dropped"
    );
    let manifest =
        super::partitioned_codec::parse_partitioned_manifest(published.manifest_bytes().as_ref())
            .expect("verified published index manifest");

    let index_segments = manifest.resolution_index.segments().count();
    assert!(
        index_segments >= 3,
        "the cold resolution index actually publishes definition, reference and alias segments"
    );
    let expected = u64::try_from(
        files.len() * 2
            + manifest.file_evidence.len()
            + manifest.code_graph_pages.len()
            + index_segments,
    )
    .expect("bounded cold resolution work");
    let observed = observer.0.lock().expect("resolution observations").clone();
    assert_eq!(
        observed,
        (1..=expected).collect::<Vec<_>>(),
        "only accepted fact, binding, evidence, index-segment and graph-page work advances the observed cursor"
    );
    assert!(
        store
            .decode_active(
                &CodeIndexGenerationScopeV1::for_snapshot(published.snapshot()),
                &operation,
            )
            .expect("read the committed cold generation")
            .is_some(),
        "counted index segments must produce a readable published generation"
    );
}

#[test]
fn rust_edits_seal_over_the_parent_like_a_cold_build() {
    assert_eq!(
        differential_counts("rust"),
        DifferentialCounts {
            sparse: 45,
            cold: 14,
            moved: 44,
        }
    );
}

#[test]
fn typescript_edits_seal_over_the_parent_like_a_cold_build() {
    assert_eq!(
        differential_counts("typescript"),
        DifferentialCounts {
            sparse: 44,
            cold: 0,
            moved: 44,
        }
    );
}

#[test]
fn python_edits_seal_over_the_parent_like_a_cold_build() {
    assert_eq!(
        differential_counts("python"),
        DifferentialCounts {
            sparse: 46,
            cold: 0,
            moved: 45,
        }
    );
}

#[test]
fn go_edits_seal_over_the_parent_like_a_cold_build() {
    assert_eq!(
        differential_counts("go"),
        DifferentialCounts {
            sparse: 37,
            cold: 29,
            moved: 37,
        }
    );
}

#[test]
fn java_edits_seal_over_the_parent_like_a_cold_build() {
    assert_eq!(
        differential_counts("java"),
        DifferentialCounts {
            sparse: 66,
            cold: 2,
            moved: 66,
        }
    );
}

#[test]
fn ruby_edits_seal_over_the_parent_like_a_cold_build() {
    assert_eq!(
        differential_counts("ruby"),
        DifferentialCounts {
            sparse: 52,
            cold: 18,
            moved: 52,
        }
    );
}

/// Fails on a restore that re-derives its generation's cross-file edges by
/// resolving the whole corpus, and on a build over a sealed parent that
/// does. Both must hold exactly the edges a whole resolution derives.
#[test]
fn a_restored_parent_and_the_edit_over_it_resolve_nothing_whole() {
    for language in ["rust", "typescript", "python", "go", "java", "ruby"] {
        let tree = fixture_files(&Path::new(FIXTURE_ROOT).join(language));
        let store = WorkerPublicationStore::default();
        let mut owner = CodeIndexProductionOwnerV1::new(worker_config(), store.clone())
            .expect("production owner");
        let (parent, parent_operation) = publish(&mut owner, &tree, None, false, 1_000_000);
        let parent = store
            .decode_active(
                &CodeIndexGenerationScopeV1::for_snapshot(parent.snapshot()),
                &parent_operation,
            )
            .expect("decode cold fixture")
            .expect("cold fixture active");
        assert!(
            parent
                .edges
                .iter()
                .any(|edge| edge.authority == EdgeAuthorityV1::NameResolved),
            "{language}: the fixture resolves cross-file edges"
        );
        super::helpers::take_seal_reference_resolutions();
        let scope = CodeIndexGenerationScopeV1::for_snapshot(parent.snapshot());
        let restored = store
            .decode_active(&scope, &parent_operation)
            .expect("the parent restores")
            .expect("an active parent");
        assert_eq!(
            super::helpers::take_seal_reference_resolutions(),
            0,
            "{language}: the restore resolved the corpus whole"
        );
        assert_eq!(restored.edges, parent.edges, "{language}: restored edges");
        assert_eq!(restored.edge_abstentions, parent.edge_abstentions);
        drop(owner);

        let mut restarted = CodeIndexProductionOwnerV1::new(worker_config(), store.clone())
            .expect("restarted production owner");
        let index = tree
            .iter()
            .position(|(path, _)| {
                !matches!(language_for(path), "toml" | "json") && !path.ends_with("go.mod")
            })
            .expect("a source file");
        let mut shifted = tree.clone();
        shifted[index].1 = format!("\n\n{}", tree[index].1);
        let (edited, edited_operation) =
            publish(&mut restarted, &shifted, Some(index), true, 1_100_000);
        assert_eq!(
            edited.cold_reason(),
            None,
            "{language}: the edit seals over its parent"
        );
        assert_eq!(
            super::helpers::take_seal_reference_resolutions(),
            0,
            "{language}: the first build after the restart resolved the corpus whole"
        );
        let edited = store
            .decode_active(&scope, &edited_operation)
            .expect("the edit restores")
            .expect("an active edit");
        let full_resolution =
            resolve_cross_file_references_charged(&edited.files, &mut |_| Ok(()), None)
                .expect("whole reference resolution");
        let (full_edges, _) =
            edge_evidence(&edited.files, full_resolution.edges).expect("whole edge evidence");
        assert_eq!(
            edited.edges, full_edges,
            "{language}: edges of the first build after the restart"
        );
    }
}
