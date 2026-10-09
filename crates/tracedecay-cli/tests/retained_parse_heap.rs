//! The retained-parse pool is charged the pages its documents occupy in the
//! shipped allocator, and releasing it returns those pages.
//!
//! One test in its own binary: it measures this process's anonymous RSS.

#![cfg(all(target_os = "linux", not(feature = "alloc-jemalloc")))]

#[path = "../src/process_allocator.rs"]
mod process_allocator;

use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_code_extraction::incremental::ParseDocumentIdentity;
use tracedecay_code_index::retained_parse::{RetainedParsePoolReleaseV1, SharedRetainedParsePool};
use tracedecay_domain::{ExtractorRevision, ProjectId, RepositoryDirtyStateV1, RepositoryId};
use tracedecay_runtime_core::resident_memory::{
    release_process_allocator_memory_v1, sampled_process_resident_bytes_v1,
};

#[global_allocator]
static MIMALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

const MIB: u64 = 1024 * 1024;
const DOCUMENTS: usize = 120;

fn identity(ordinal: usize) -> ParseDocumentIdentity {
    ParseDocumentIdentity::Repository {
        project_id: ProjectId::new("project.retained-heap").expect("project"),
        repository_id: RepositoryId::new("repository.retained-heap").expect("repository"),
        worktree_id: None,
        reference: None,
        commit: None,
        tree: None,
        dirty: RepositoryDirtyStateV1::Dirty,
        logical_path: format!("src/module_{ordinal}.rs"),
    }
}

fn anon_bytes() -> u64 {
    let _ = release_process_allocator_memory_v1();
    sampled_process_resident_bytes_v1().expect("this kernel reports RssAnon")
}

#[test]
fn retained_parses_are_charged_what_releasing_them_returns() {
    process_allocator::configure_process_allocator();
    let pool = SharedRetainedParsePool::default();
    let registry = LanguageRegistry::new();
    let extractor = registry.extractor_for_file("src/lib.rs").expect("rust");
    let revision = ExtractorRevision::new("extractor.rust.retained-heap").expect("revision");
    let source = (0..400)
        .map(|index| {
            format!("pub fn f{index}(v: &[u32]) -> u32 {{ v.iter().map(|x| x * {index}).sum() }}\n")
        })
        .collect::<String>();

    pool.parse_and_extract_artifact_unretained_with_control(
        identity(0),
        "rust",
        &source,
        extractor,
        None,
    )
    .expect("unretained parse");
    assert_eq!(pool.holding(), None, "an unretained parse holds nothing");

    for ordinal in 0..DOCUMENTS {
        let (_, extraction) = pool
            .parse_and_extract_artifact_for_revision_with_control(
                identity(ordinal),
                "rust",
                &source,
                extractor,
                &revision,
                None,
            )
            .expect("retained parse");
        drop(extraction);
    }
    let held = pool.holding().expect("retained documents");
    assert_eq!(held.documents, DOCUMENTS);
    let charged = held.bytes.expect("the shipped allocator measures the pool");

    let before = anon_bytes();
    assert_eq!(
        pool.release(),
        RetainedParsePoolReleaseV1::Released {
            bytes: Some(charged)
        }
    );
    let freed = before.saturating_sub(anon_bytes());
    assert_eq!(pool.holding(), None);
    assert!(
        freed >= 16 * MIB,
        "releasing {DOCUMENTS} retained documents returned {freed} bytes"
    );
    assert!(
        charged * 10 >= freed * 8 && charged * 10 <= freed * 12,
        "the pool was charged {charged} bytes and releasing it returned {freed}"
    );
}

/// Reproducible idle-after-ingest heap: a transient payload is charged at
/// peak and the shipped allocator returns it on collect. Prints the
/// before/peak/after numbers the PR body cites.
#[test]
fn idle_collect_returns_a_transient_ingest_heap() {
    process_allocator::configure_process_allocator();
    const CHUNKS: usize = 64;
    const CHUNK_BYTES: usize = 1024 * 1024;
    let before = anon_bytes();
    let payload = (0..CHUNKS)
        .map(|_| vec![7_u8; CHUNK_BYTES])
        .collect::<Vec<_>>();
    let peak = anon_bytes();
    assert!(
        peak >= before + 32 * MIB,
        "the ingest-shaped payload must be visible in RssAnon: before={before} peak={peak}"
    );
    drop(payload);
    let _ = release_process_allocator_memory_v1();
    let after = anon_bytes();
    let returned = peak.saturating_sub(after);
    eprintln!("HEAP_PROOF before={before} peak={peak} after={after} returned={returned}");
    assert!(
        after <= peak.saturating_sub(16 * MIB),
        "idle collect must return the transient heap: before={before} peak={peak} after={after}"
    );
}
