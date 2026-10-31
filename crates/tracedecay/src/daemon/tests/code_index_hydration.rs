use std::fmt::Write;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::{
    AuthorizationRevision, CodeSearchChunkId, ComponentRevision, ExactClass, FreshnessVectorDigest,
    FusedCandidate, FusionProfileId, LogicalEvidenceId, OccurrenceProvenance, PrincipalId,
    RankedCandidate, RetrievalAnchorId, RetrievalBudget, RetrievalRequest, RetrievalScope,
    RetrievalSnapshot, RetrievalSourceRoleV1, SingleRootScopeV1, SourceFreshness,
    SourceOccurrenceId, TemporalModeV1, VectorWatermark,
};

use super::git;
use tracedecay_code_index_runtime::code_index_executor::{
    code_index_search_display_binding, code_index_text_search_display_binding,
};
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexWorktreeSchedulerV1, LatestCompleteCodeIndexV1, SharedCodeIndexBytePoolV1,
};
/// A sealed production generation over a committed `src/lib.rs`. Fields drop
/// in reverse of construction order.
struct ProductionGeneration {
    latest: LatestCompleteCodeIndexV1,
    _scheduler: CodeIndexWorktreeSchedulerV1,
    _store: TempDir,
    _project: TempDir,
}

fn production_generation(lib_rs: &str) -> ProductionGeneration {
    let project = TempDir::new().expect("project");
    git(project.path(), &["init", "-q", "-b", "main"]);
    git(project.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        project.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(project.path().join("src")).expect("source directory");
    std::fs::write(project.path().join("src/lib.rs"), lib_rs).expect("source");
    git(project.path(), &["add", "."]);
    git(project.path(), &["commit", "-qm", "fixture"]);

    let store = TempDir::new().expect("store");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        tracedecay_domain::ProjectId::new("project.code-index-hydration").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("scheduler");
    scheduler.reconcile_now().expect("publish generation");
    let latest = scheduler.latest_complete().expect("complete generation");
    ProductionGeneration {
        latest,
        _scheduler: scheduler,
        _store: store,
        _project: project,
    }
}

fn production_freshness(latest: &LatestCompleteCodeIndexV1) -> SourceFreshness {
    tracedecay_query::retrieval::graph::production_code_index_freshness(
        latest.generation().manifest().seal.sealed_at,
        ComponentRevision::new("policy.code-index.daemon.v1").expect("policy revision"),
    )
    .expect("freshness")
}

/// A single-result request over the sealed generation's snapshot, its
/// principal, authorization, and profile named after `label`.
fn production_request(latest: &LatestCompleteCodeIndexV1, label: &str) -> RetrievalRequest {
    let generation = latest.generation();
    RetrievalRequest {
        principal: PrincipalId::new(format!("principal.{label}")).expect("principal"),
        scope: RetrievalScope {
            privacy_domain: generation.manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: generation.snapshot().repository.clone(),
                worktree: generation.snapshot().worktree.clone(),
                reference: generation.snapshot().reference.clone(),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(
                generation.manifest().snapshot_digest.as_str(),
            )
            .expect("snapshot freshness"),
            authorization_revision: AuthorizationRevision::new(format!("authorization.{label}"))
                .expect("authorization revision"),
            captured_at: generation.manifest().seal.sealed_at,
        },
        profile_id: FusionProfileId::new(format!("profile.{label}")).expect("profile"),
        budget: RetrievalBudget {
            max_candidates_per_lane: 1,
            max_fused_candidates: 1,
            max_hydrated_results: 1,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        },
    }
}

/// A symbol-anchored hit in the lexical-lane format: the matched `chunk`
/// rides the provenance as `code-chunk:<generation>:<chunk_id>`.
fn symbol_chunk_hit(
    latest: &LatestCompleteCodeIndexV1,
    symbol_occurrence: &str,
    chunk: &tracedecay_domain::CodeSearchChunkV1,
    freshness: &SourceFreshness,
) -> OccurrenceProvenance {
    let generation = latest.generation();
    OccurrenceProvenance {
        source_occurrence_id: SourceOccurrenceId::new(format!(
            "code-chunk:{}:{}",
            generation.manifest().generation_id.as_str(),
            chunk.id.as_str()
        ))
        .expect("chunk source occurrence"),
        file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
        retriever_evidence_anchor: RetrievalAnchorId::new(format!(
            "code-symbol:{symbol_occurrence}"
        ))
        .expect("symbol evidence"),
        source_namespace: freshness.source_namespace.clone(),
        repository_id: Some(generation.snapshot().repository.clone()),
        session_or_thread_id: None,
        logical_copy_cluster_id: None,
        logical_copy_evidence_anchor: None,
        evidence_role: tracedecay_domain::EvidenceRole::Primary,
        freshness: freshness.clone(),
    }
}

#[test]
fn production_chunk_candidate_hydrates_from_frozen_generation() {
    let fixture = production_generation("pub fn chunk_target() -> u32 { 7 }\n");
    let generation = fixture.latest.generation();
    let chunk = generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.anchor.symbol_occurrence_id.is_some())
        .expect("symbol-backed production chunk");
    let chunk_id = CodeSearchChunkId::new(chunk.id.as_str().to_owned()).expect("chunk id");
    let anchor =
        RetrievalAnchorId::new(format!("code-chunk:{}", chunk_id.as_str())).expect("chunk anchor");
    let source_occurrence = SourceOccurrenceId::new(format!("code-chunk:{}", chunk_id.as_str()))
        .expect("chunk source occurrence");
    let freshness = production_freshness(&fixture.latest);
    let candidate = RankedCandidate {
        candidate: FusedCandidate {
            anchor_id: anchor.clone(),
            logical_evidence_id: LogicalEvidenceId::new(anchor.as_str().to_owned())
                .expect("logical evidence"),
            occurrences: vec![OccurrenceProvenance {
                source_occurrence_id: source_occurrence.clone(),
                file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
                retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                    "code-chunk:{}",
                    chunk_id.as_str()
                ))
                .expect("chunk evidence"),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(generation.snapshot().repository.clone()),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: tracedecay_domain::EvidenceRole::Primary,
                freshness: freshness.clone(),
            }],
            exact_class: ExactClass::Approximate,
            source_role: RetrievalSourceRoleV1::default(),
            utility_micros: 1,
            contributions: Vec::new(),
            freshness: vec![freshness.clone()],
            decisions: Vec::new(),
        },
        final_ordinal: 0,
    };
    let request = production_request(&fixture.latest, "chunk-hydration");

    let display_paths =
        tracedecay_code_index_runtime::code_index_executor::CodeIndexDisplayPathIndexV1::for_generation(generation)
            .expect("display path index for the sealed generation");
    let (display, provenance) =
        code_index_search_display_binding(generation, &display_paths, &request, &candidate)
            .expect("frozen chunk hydration");
    let symbol_occurrence = chunk
        .anchor
        .symbol_occurrence_id
        .as_ref()
        .expect("symbol occurrence");
    let symbol = generation
        .symbols()
        .symbols
        .iter()
        .find(|symbol| symbol.occurrence == *symbol_occurrence)
        .expect("indexed symbol");
    assert_eq!(display.qualified_name, symbol.qualified_name);
    assert_eq!(display.kind, symbol.kind);
    let declaring_file = generation
        .snapshot()
        .files
        .iter()
        .find(|file| file.logical_path == display.path)
        .expect("display path names a snapshot file");
    assert_eq!(
        declaring_file.disposition,
        tracedecay_domain::SnapshotFileDispositionV1::Present
    );
    assert_eq!(provenance.source_occurrence_id, source_occurrence);
    assert_eq!(
        candidate.candidate.occurrences[0].freshness,
        provenance.freshness
    );
    assert_eq!(
        candidate.candidate.occurrences[0].source_namespace,
        provenance.source_namespace
    );
}

#[test]
fn production_symbol_candidate_binds_the_attributed_chunk_window() {
    const BODY: &str = "pub fn chunk_target() -> u32 {\n    7\n}\n";
    let fixture = production_generation(BODY);
    let generation = fixture.latest.generation();
    let chunk = generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.sanitized_text.as_str().contains("    7"))
        .expect("symbol body production chunk");
    let symbol_occurrence = chunk
        .anchor
        .symbol_occurrence_id
        .as_ref()
        .expect("symbol occurrence")
        .clone();
    let anchor = RetrievalAnchorId::new(format!("code-symbol:{}", symbol_occurrence.as_str()))
        .expect("symbol anchor");
    let freshness = production_freshness(&fixture.latest);
    let candidate = RankedCandidate {
        candidate: FusedCandidate {
            anchor_id: anchor.clone(),
            logical_evidence_id: LogicalEvidenceId::new(anchor.as_str().to_owned())
                .expect("logical evidence"),
            occurrences: vec![symbol_chunk_hit(
                &fixture.latest,
                symbol_occurrence.as_str(),
                chunk,
                &freshness,
            )],
            exact_class: ExactClass::Approximate,
            source_role: RetrievalSourceRoleV1::default(),
            utility_micros: 1,
            contributions: Vec::new(),
            freshness: vec![freshness.clone()],
            decisions: Vec::new(),
        },
        final_ordinal: 0,
    };
    let request = production_request(&fixture.latest, "symbol-hydration");

    let display_paths =
        tracedecay_code_index_runtime::code_index_executor::CodeIndexDisplayPathIndexV1::for_generation(generation)
            .expect("display path index for the sealed generation");
    let (display, _provenance) =
        code_index_search_display_binding(generation, &display_paths, &request, &candidate)
            .expect("frozen symbol hydration");
    let site = display.site.expect("lineage site");
    let tracedecay_query::code_search::CodeIndexSearchSiteV1::SymbolLines { code_window, .. } =
        site
    else {
        panic!("symbol anchor must carry a lineage site, got {site:?}");
    };
    let window = code_window.expect("symbol chunk window bound");
    assert_eq!(display.name, "chunk_target");
    assert_eq!(display.qualified_name, "src/lib.rs::chunk_target");
    assert_eq!(display.kind, "function");
    assert_eq!(display.path, "src/lib.rs");
    assert_eq!(window.sanitized_text.as_str(), BODY);
}

#[test]
fn production_symbol_candidate_serves_the_matched_body_piece() {
    // A symbol larger than 64KiB splits into fallback-window pieces. Unique
    // identifiers mark the opening window and a later piece so the binding
    // can be checked by the text it serves, not by chunk metadata.
    const FIRST_WINDOW: &str = "window_first_unique";
    const MATCHED_WINDOW: &str = "window_matched_unique";
    let mut lib_rs = format!("pub fn chunk_target() -> u32 {{\n    let {FIRST_WINDOW} = 1u32;\n");
    while lib_rs.len() < 20_000 {
        lib_rs.push_str("    let _pad = 1u32;\n");
    }
    writeln!(lib_rs, "    let {MATCHED_WINDOW} = 2u32;").expect("matched window fixture");
    while lib_rs.len() < 70_000 {
        lib_rs.push_str("    let _pad = 2u32;\n");
    }
    write!(lib_rs, "    {FIRST_WINDOW}\n}}\n").expect("fixture closing body");
    let fixture = production_generation(&lib_rs);
    let latest = &fixture.latest;
    let generation = latest.generation();

    let signature_chunk = generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.sanitized_text.as_str() == "pub fn chunk_target() -> u32 {")
        .expect("signature chunk");
    let symbol_occurrence = signature_chunk
        .anchor
        .symbol_occurrence_id
        .clone()
        .expect("signature names a symbol");
    let first_piece = generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| {
            chunk
                .sanitized_text
                .as_str()
                .contains(&format!("let {FIRST_WINDOW}"))
        })
        .expect("opening body piece");
    let matched_piece = generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.sanitized_text.as_str().contains(MATCHED_WINDOW))
        .expect("later body piece");

    let anchor = RetrievalAnchorId::new(format!("code-symbol:{}", symbol_occurrence.as_str()))
        .expect("symbol anchor");
    let freshness = production_freshness(latest);
    let chunk_hit = |chunk: &tracedecay_domain::CodeSearchChunkV1| {
        symbol_chunk_hit(latest, symbol_occurrence.as_str(), chunk, &freshness)
    };
    // Fusion orders occurrences by id, so the signature hit can precede
    // the body piece the lane matched; arrange that order directly.
    let candidate = RankedCandidate {
        candidate: FusedCandidate {
            anchor_id: anchor.clone(),
            logical_evidence_id: LogicalEvidenceId::new(anchor.as_str().to_owned())
                .expect("logical evidence"),
            occurrences: vec![chunk_hit(signature_chunk), chunk_hit(matched_piece)],
            exact_class: ExactClass::Approximate,
            source_role: RetrievalSourceRoleV1::default(),
            utility_micros: 1,
            contributions: Vec::new(),
            freshness: vec![freshness.clone()],
            decisions: Vec::new(),
        },
        final_ordinal: 0,
    };
    let request = production_request(latest, "symbol-hydration");

    let display_paths =
        tracedecay_code_index_runtime::code_index_executor::CodeIndexDisplayPathIndexV1::for_generation(generation)
            .expect("display path index for the sealed generation");
    let (display, _provenance) =
        code_index_search_display_binding(generation, &display_paths, &request, &candidate)
            .expect("frozen symbol hydration");
    let site = display.site.expect("lineage site");
    let tracedecay_query::code_search::CodeIndexSearchSiteV1::SymbolLines { code_window, .. } =
        site
    else {
        panic!("symbol anchor must carry a lineage site, got {site:?}");
    };
    let window = code_window.expect("symbol chunk window bound");
    assert_eq!(display.name, "chunk_target");
    assert_eq!(display.path, "src/lib.rs");
    assert_eq!(
        window.sanitized_text.as_str(),
        matched_piece.sanitized_text.as_str()
    );
    assert!(
        window.sanitized_text.as_str().contains(MATCHED_WINDOW),
        "complete lane must serve the later piece"
    );
    assert!(
        !window.sanitized_text.as_str().contains(FIRST_WINDOW),
        "complete lane must not fall back to the opening piece"
    );

    let first_only = RankedCandidate {
        candidate: FusedCandidate {
            occurrences: vec![chunk_hit(first_piece)],
            ..candidate.candidate.clone()
        },
        final_ordinal: 0,
    };
    let (first_display, _) =
        code_index_search_display_binding(generation, &display_paths, &request, &first_only)
            .expect("opening piece hydration");
    let first_site = first_display.site.expect("opening lineage site");
    let tracedecay_query::code_search::CodeIndexSearchSiteV1::SymbolLines {
        code_window: Some(first_window),
        ..
    } = first_site
    else {
        panic!("opening piece must carry a lineage site, got {first_site:?}");
    };
    assert_eq!(
        first_window.sanitized_text.as_str(),
        first_piece.sanitized_text.as_str()
    );
    assert!(
        first_window.sanitized_text.as_str().contains(FIRST_WINDOW),
        "attributed opening piece must serve its own text"
    );

    let text = latest.text_generation_handle();
    text.production_query_owners().expect("text serving owners");
    let (display, _provenance) =
        code_index_text_search_display_binding(&text, &request, &candidate)
            .expect("text generation symbol hydration");
    let site = display.site.expect("artifact site");
    let tracedecay_query::code_search::CodeIndexSearchSiteV1::ArtifactWindow(window) = site else {
        panic!("text lane must carry an artifact window, got {site:?}");
    };
    assert_eq!(display.name, "chunk_target");
    assert_eq!(display.path, "src/lib.rs");
    assert_eq!(
        window.sanitized_text.as_str(),
        matched_piece.sanitized_text.as_str()
    );
    assert!(
        window.sanitized_text.as_str().contains(MATCHED_WINDOW),
        "text lane must serve the later piece"
    );
    assert!(
        !window.sanitized_text.as_str().contains(FIRST_WINDOW),
        "text lane must not fall back to the opening piece"
    );
}
