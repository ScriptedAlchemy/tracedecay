use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use tracedecay_code_index::chunks::{
    DeterministicCodeChunker, ExtractionAdmittedCodeSearchChunkV1, content_digest,
};
use tracedecay_code_index::extract::{LanguageExtractor, NeverCancelled, TreeSitterExtractor};
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_domain::{
    BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1, ComponentRevision, ContentDigest,
    EphemeralSanitizedQueryViewV1, ExactAdmissionRuleRevision, ExactAdmissionValidator,
    ExactFieldV1, ExactTechnicalTermKindV1, ExactTechnicalTermV1, FileOccurrenceId,
    FreshnessCompatibilityV1, LanguageDescriptorRevision, PolicyRevisionId, PrincipalId, ProjectId,
    QueryNormalizationRevision, RepositoryId, RetrievalBudget, RetrievalRequest, RetrievalScope,
    RetrievalSnapshot, RetrieverOutcome, SanitizationReceiptId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, ScoreDomainId, SensitivityDecision,
    SensitivityLevelV1, SingleRootScopeV1, SnapshotFileDispositionV1, SourceFreshness,
    SourceInstanceKey, SourceNamespace, SourceSpan, SymbolOccurrenceId, TemporalModeV1, UtcMicros,
    ValidatedCodeFileV1, VectorWatermark,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalProjectionAdapterV1, CodeLexicalProjectionMetadataV1, LexicalFieldFilterV1,
    LexicalFieldV1, LexicalLane, LexicalLaneRequest, LexicalLaneRetriever,
    MAX_FUZZY_TERM_EXPANSIONS_V1, MAX_LEXICAL_QUERY_TERM_BYTES_V1,
};

pub(crate) fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

pub(crate) fn digest_id<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid fixture digest")
}

pub(crate) fn budget(max_candidates_per_lane: u32) -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane,
        max_fused_candidates: 16,
        max_hydrated_results: 8,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

pub(crate) fn base_request(_query: &str, max_candidates_per_lane: u32) -> RetrievalRequest {
    RetrievalRequest {
        principal: id::<PrincipalId>("principal.fixture"),
        scope: RetrievalScope {
            privacy_domain: id("privacy.fixture"),
            root: SingleRootScopeV1 {
                repository: id("repository.fixture"),
                worktree: None,
                reference: None,
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: digest_id('f'),
            authorization_revision: id("authorization.v1"),
            captured_at: UtcMicros(7),
        },
        profile_id: id("profile.fixture.v1"),
        budget: budget(max_candidates_per_lane),
    }
}

pub(crate) fn query_view(query: &str) -> EphemeralSanitizedQueryViewV1 {
    EphemeralSanitizedQueryViewV1::sanitize(
        query,
        id::<SanitizerRevision>("query-sanitizer.v1"),
        id::<QueryNormalizationRevision>("query-normalization.v1"),
    )
    .expect("query sanitizes")
}

pub(crate) fn freshness(compatibility: FreshnessCompatibilityV1) -> SourceFreshness {
    SourceFreshness {
        source_namespace: id::<SourceNamespace>("ns.code.fixture"),
        source_instance: id::<SourceInstanceKey>("instance.fixture"),
        source_watermark: Some(7),
        projection_watermark: Some(7),
        observed_at: UtcMicros(7),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility,
        policy_revision: id("policy.fixture.v1"),
    }
}

pub(crate) fn projection_metadata(
    generation: &CodeGenerationId,
    compatibility: FreshnessCompatibilityV1,
) -> CodeLexicalProjectionMetadataV1 {
    let mut logical_paths = (0..=128)
        .map(|ordinal| {
            (
                id::<FileOccurrenceId>(&format!("file.{ordinal}")),
                format!("src/file-{ordinal}.rs"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    logical_paths.extend((0..=128).map(|ordinal| {
        (
            id::<FileOccurrenceId>(&format!("file.admitted.{ordinal}")),
            format!("src/admitted_{ordinal}.rs"),
        )
    }));
    CodeLexicalProjectionMetadataV1 {
        generation: generation.clone(),
        repository_id: Some(id::<RepositoryId>("repository.fixture")),
        logical_paths,
        freshness: freshness(compatibility),
        exact_retriever_revision: id::<ComponentRevision>("retriever.exact.v1"),
        lexical_retriever_revision: id::<ComponentRevision>("retriever.lexical.v1"),
        exact_score_domain: id::<ScoreDomainId>("score.exact.v1"),
    }
}

pub(crate) fn chunk(
    generation: &CodeGenerationId,
    ordinal: u32,
    grain: CodeSearchChunkGrainV1,
    text: &str,
    terms: &[(ExactTechnicalTermKindV1, &str)],
    subtokens: &[&str],
) -> CodeSearchChunkV1 {
    let symbol = matches!(
        grain,
        CodeSearchChunkGrainV1::SymbolSignature
            | CodeSearchChunkGrainV1::SymbolBody
            | CodeSearchChunkGrainV1::SymbolMember
    )
    .then(|| id::<SymbolOccurrenceId>(&format!("symbol.{ordinal}")));
    let mut exact_terms: Vec<ExactTechnicalTermV1> = terms
        .iter()
        .map(|(kind, term)| {
            let start = text
                .find(term)
                .unwrap_or_else(|| panic!("term {term:?} is present in {text:?}"));
            let span = SourceSpan {
                start_byte: start as u64,
                end_byte: (start + term.len()) as u64,
            };
            if *kind == ExactTechnicalTermKindV1::WholeSymbol {
                ExactTechnicalTermV1::untrusted_whole_symbol_candidate(
                    term.as_bytes().to_vec(),
                    span,
                    symbol.clone().expect("symbol grain"),
                )
            } else if matches!(
                kind,
                ExactTechnicalTermKindV1::CompilerErrorText
                    | ExactTechnicalTermKindV1::RuntimeErrorText
            ) {
                ExactTechnicalTermV1::untrusted_contextual_text_candidate(
                    *kind,
                    term.as_bytes().to_vec(),
                    span,
                )
            } else {
                ExactTechnicalTermV1::technical(*kind, term.as_bytes().to_vec(), span)
            }
            .expect("valid exact-term fixture")
        })
        .collect();
    exact_terms.sort_by(|left, right| {
        (
            left.span().start_byte,
            left.span().end_byte,
            left.kind(),
            left.canonical_bytes(),
            left.original_bytes(),
        )
            .cmp(&(
                right.span().start_byte,
                right.span().end_byte,
                right.kind(),
                right.canonical_bytes(),
                right.original_bytes(),
            ))
    });
    CodeSearchChunkV1 {
        id: id::<CodeSearchChunkId>(&format!("chunk.{ordinal}")),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation.clone(),
            file_occurrence_id: id::<FileOccurrenceId>(&format!("file.{ordinal}")),
            symbol_occurrence_id: symbol,
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: text.len() as u64,
            },
            grain,
            ordinal,
        },
        content_digest: digest_id::<ContentDigest>(
            char::from_digit((ordinal % 10) + 1, 16).expect("hex digit"),
        ),
        language_descriptor_revision: id::<LanguageDescriptorRevision>("language.rust.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: id("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Internal,
            policy_revision: id::<PolicyRevisionId>("policy.fixture.v1"),
        },
        exact_terms,
        subtokens: subtokens.iter().map(|value| (*value).to_owned()).collect(),
        sanitized_text: BoundedSanitizedText::new(text).expect("bounded fixture text"),
    }
}

fn admitted_rust_chunk(
    generation: &CodeGenerationId,
    ordinal: u32,
    source: &str,
    grain: CodeSearchChunkGrainV1,
    symbol_name: &str,
) -> ExtractionAdmittedCodeSearchChunkV1 {
    let registry = StaticLanguageRegistry::new();
    let descriptor = registry
        .descriptor(&id("rust"))
        .expect("rust descriptor")
        .clone();
    let sanitizer_revision = id::<SanitizerRevision>("sanitizer.v1");
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: id(&format!("file.admitted.{ordinal}")),
        logical_path: format!("src/admitted_{ordinal}.rs"),
        language: Some(id("rust")),
        content_digest: content_digest(source.as_bytes()),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let intake =
        SanitizedCodeIntake::new(registry, sanitizer_revision.clone(), UtcMicros(1_000_000));
    let snapshot = intake
        .admit(SanitizedCodeSnapshotV1 {
            repository: id("repo.fixture"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: sanitizer_revision.clone(),
            sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.fixture")],
            content_identity: content_digest(source.as_bytes()),
            captured_at: UtcMicros(1_000_000),
            files: vec![file.clone()],
        })
        .expect("snapshot admission");
    let file = intake
        .bind_file(
            &snapshot,
            &id::<ProjectId>("project.fixture"),
            ValidatedCodeFileV1 {
                generation_id: generation.clone(),
                file,
                snapshot_digest: snapshot.snapshot().intake_digest.clone(),
                sanitized_bytes: source.as_bytes().to_vec(),
            },
        )
        .expect("file admission");
    let batch = TreeSitterExtractor::new()
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("extract rust fixture");
    let chunker = DeterministicCodeChunker::new(
        generation.clone(),
        id("repo.fixture"),
        sanitizer_revision,
        id("policy.fixture.v1"),
        id("chunker.v1"),
        tracedecay_code_extraction::LanguageRegistry::new(),
    );
    let (artifacts, authority) = chunker
        .index_file_with_authority_from_extraction(
            &file,
            &batch,
            &descriptor,
            SensitivityLevelV1::Public,
            &NeverCancelled,
        )
        .expect("chunk with exact authority");
    let chunk = artifacts
        .chunks
        .chunks
        .into_iter()
        .find(|chunk| {
            chunk.anchor.grain == grain
                && chunk.exact_terms.iter().any(|term| {
                    term.kind() == ExactTechnicalTermKindV1::WholeSymbol
                        && term.original_bytes() == symbol_name.as_bytes()
                })
        })
        .expect("requested parser-minted symbol chunk");
    authority.admit(chunk).expect("exact extraction admission")
}

pub(crate) fn lexical_request(
    query: &str,
    whole_terms: &[&str],
    subtokens: &[&str],
    phrases: &[&str],
    fuzzy_budget: u32,
    max_candidates: u32,
) -> LexicalLaneRequest<'static> {
    let query_view = Box::leak(Box::new(query_view(query)));
    LexicalLaneRequest {
        base: base_request(query, max_candidates),
        query_view,
        generation: id("generation.1"),
        whole_terms: whole_terms.iter().map(|term| (*term).to_owned()).collect(),
        subtokens: subtokens.iter().map(|term| (*term).to_owned()).collect(),
        phrases: phrases.iter().map(|term| (*term).to_owned()).collect(),
        field_filters: Vec::<LexicalFieldFilterV1>::new(),
        fuzzy_budget,
        lexical_profile_revision: id("lexical-profile.v1"),
        score_domain: id("score.lexical.v1"),
        budget: budget(max_candidates),
    }
}

pub(crate) fn complete<T: fmt::Debug>(outcome: RetrieverOutcome<T>) -> T {
    match outcome {
        RetrieverOutcome::Complete(value) => value,
        other => panic!("expected complete retrieval, got {other:?}"),
    }
}

#[test]
fn matching_symbol_occurrence_does_not_admit_raw_or_json_exact_terms() {
    let generation = id::<CodeGenerationId>("generation.1");
    let raw = chunk(
        &generation,
        1,
        CodeSearchChunkGrainV1::SymbolSignature,
        "fn forged_symbol",
        &[(ExactTechnicalTermKindV1::WholeSymbol, "forged_symbol")],
        &["forged", "symbol"],
    );
    assert_eq!(
        raw.exact_terms[0].symbol_occurrence_id(),
        raw.anchor.symbol_occurrence_id.as_ref()
    );
    let metadata = projection_metadata(&generation, FreshnessCompatibilityV1::Current);
    assert!(
        CodeLexicalProjectionAdapterV1::new(metadata.clone(), vec![raw.clone()]).is_err(),
        "public raw-parts construction cannot admit WholeSymbol evidence"
    );

    let decoded: CodeSearchChunkV1 =
        serde_json::from_slice(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(
        decoded.exact_terms[0].symbol_occurrence_id(),
        decoded.anchor.symbol_occurrence_id.as_ref()
    );
    assert!(
        CodeLexicalProjectionAdapterV1::new(metadata, vec![decoded]).is_err(),
        "JSON chunks remain untrusted even when occurrence ids match"
    );
}

#[test]
fn central_exact_authority_classifies_every_protected_term() {
    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    let query = r#"reserve_stock std::collections::HashMap src/main.rs "connection refused" error:"socket closed" E0308 --release cargo tracedecay.data.dir deadbee"#;
    let request = base_request(query, 16);
    let query_view = query_view(query);

    let literals = authority.parse_literals(&query_view, &request);
    let fields: BTreeSet<ExactFieldV1> = literals.iter().map(|literal| literal.field).collect();

    assert_eq!(
        fields,
        BTreeSet::from([
            ExactFieldV1::Identifier,
            ExactFieldV1::QualifiedName,
            ExactFieldV1::Path,
            ExactFieldV1::QuotedPhrase,
            ExactFieldV1::DiagnosticCode,
            ExactFieldV1::CompilerOrRuntimeError,
            ExactFieldV1::CliFlag,
            ExactFieldV1::ToolName,
            ExactFieldV1::ConfigurationKey,
            ExactFieldV1::CommitIdentifier,
        ])
    );
    for literal in literals {
        let proof = authority
            .admit(literal.field, &literal.original_bytes, &request)
            .expect("admission is evaluated")
            .expect("parsed protected literal is admitted");
        assert_eq!(proof.canonical_bytes, literal.canonical_bytes);
        proof
            .validate_for_request(&request)
            .expect("proof binds the request");
    }
    assert!(
        authority
            .admit(ExactFieldV1::Path, b"not a path", &request)
            .expect("invalid path is evaluated")
            .is_none()
    );
}

#[test]
fn exact_projection_emits_only_authority_minted_proofs() {
    let generation = id::<CodeGenerationId>("generation.1");
    let text = "std::collections::HashMap src/main.rs E0308 --release cargo tracedecay.data.dir commit:deadbee";
    let source = chunk(
        &generation,
        1,
        CodeSearchChunkGrainV1::SymbolBody,
        text,
        &[
            (
                ExactTechnicalTermKindV1::QualifiedName,
                "std::collections::HashMap",
            ),
            (ExactTechnicalTermKindV1::Path, "src/main.rs"),
            (ExactTechnicalTermKindV1::CompilerErrorCode, "E0308"),
            (ExactTechnicalTermKindV1::CliFlag, "--release"),
            (ExactTechnicalTermKindV1::ToolName, "cargo"),
            (
                ExactTechnicalTermKindV1::ConfigurationKey,
                "tracedecay.data.dir",
            ),
            (ExactTechnicalTermKindV1::CommitIdentifier, "commit:deadbee"),
        ],
        &["reserve", "stock"],
    );
    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    let projection = CodeLexicalProjectionAdapterV1::new(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        vec![source],
    )
    .expect("projection builds");
    let query = r#"std::collections::HashMap src/main.rs E0308 --release cargo tracedecay.data.dir commit:deadbee"#;
    let base = base_request(query, 16);
    let query_view = query_view(query);
    let request = ExactLaneRequest {
        literals: authority.parse_literals(&query_view, &base),
        base,
        query_view: &query_view,
        generation,
        budget: budget(16),
    };
    let lane = ExactLane::new(authority.clone(), projection.exact_adapter(authority));

    let batch = complete(
        lane.retrieve_exact(&request)
            .expect("exact projection query succeeds"),
    );

    assert_eq!(batch.candidates.len(), 1);
    assert_eq!(batch.coverage.examined, 1);
    assert_eq!(batch.coverage.eligible, 1);
    assert_eq!(batch.coverage.excluded, 0);
    let candidate = &batch.candidates[0];
    let proof = candidate
        .exact_admission_proof
        .as_ref()
        .expect("exact candidate carries an authority proof");
    proof
        .validate_for_request(&request.base)
        .expect("proof remains request-bound");
    let evidence = &batch.evidence_by_occurrence[&candidate.source_occurrence_id];
    assert_eq!(evidence.matched_literals.len(), 7);
}

#[test]
fn exact_projection_matches_typescript_and_csharp_diagnostic_codes() {
    let generation = id::<CodeGenerationId>("generation.1");
    let source = chunk(
        &generation,
        1,
        CodeSearchChunkGrainV1::SymbolBody,
        "TS1234 CS5678",
        &[
            (ExactTechnicalTermKindV1::CompilerErrorCode, "TS1234"),
            (ExactTechnicalTermKindV1::CompilerErrorCode, "CS5678"),
        ],
        &[],
    );
    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    let projection = CodeLexicalProjectionAdapterV1::new(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        vec![source],
    )
    .expect("projection builds");
    let query = "ts1234 cs5678";
    let base = base_request(query, 8);
    let query_view = query_view(query);
    let request = ExactLaneRequest {
        literals: authority.parse_literals(&query_view, &base),
        base,
        query_view: &query_view,
        generation,
        budget: budget(8),
    };

    let batch = complete(
        ExactLane::new(authority.clone(), projection.exact_adapter(authority))
            .retrieve_exact(&request)
            .expect("diagnostic retrieval succeeds"),
    );

    assert_eq!(batch.candidates.len(), 1);
    assert_eq!(
        batch.evidence_by_occurrence[&batch.candidates[0].source_occurrence_id]
            .matched_literals
            .len(),
        2
    );
}

#[test]
fn fielded_bm25_keeps_whole_identifiers_and_subtokens_distinct() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = vec![
        admitted_rust_chunk(
            &generation,
            1,
            "pub fn reserve_stock() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "reserve_stock",
        ),
        admitted_rust_chunk(
            &generation,
            2,
            "pub fn reserve() { let stock_inventory = 1; }\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "reserve",
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
    )
    .expect("projection builds");
    let whole_request = lexical_request("reserve_stock", &["reserve_stock"], &[], &[], 0, 8);

    let whole = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&whole_request)
            .expect("whole-term retrieval succeeds"),
    );

    assert_eq!(whole.candidates.len(), 1);
    let evidence = &whole.evidence_by_occurrence[&whole.candidates[0].source_occurrence_id];
    assert!(
        evidence
            .matched_whole_terms
            .contains(&"reserve_stock".to_owned())
    );
    assert!(evidence.matched_subtokens.is_empty());
    assert!(
        evidence
            .field_scores_micros
            .iter()
            .any(|(field, _)| *field == LexicalFieldV1::SymbolName)
    );

    let whole_subtoken_text = lexical_request("reserve", &["reserve"], &[], &[], 0, 8);
    let whole_only = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&whole_subtoken_text)
            .expect("whole-term/subtoken boundary retrieval succeeds"),
    );
    assert_eq!(
        whole_only.candidates.len(),
        1,
        "a whole-term query must not consume the distinct subtoken field"
    );

    let subtoken_request = lexical_request("reserve", &[], &["reserve"], &[], 0, 8);
    let subtokens = complete(
        LexicalLane::new(projection)
            .retrieve_lexical(&subtoken_request)
            .expect("subtoken retrieval succeeds"),
    );
    assert_eq!(subtokens.candidates.len(), 2);
    assert!(subtokens.evidence_by_occurrence.values().all(|evidence| {
        evidence.matched_whole_terms.is_empty()
            && evidence.matched_subtokens == vec!["reserve".to_owned()]
    }));
}

#[test]
fn lexical_phrase_and_bounded_fuzzy_recovery_are_deterministic() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = vec![
        admitted_rust_chunk(
            &generation,
            1,
            "pub fn reserve() { // reserve stock inventory\n}\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "reserve",
        ),
        admitted_rust_chunk(
            &generation,
            2,
            "pub fn reserve_stock() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "reserve_stock",
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
    )
    .expect("projection builds");
    let phrase_request = lexical_request(r#""reserve stock""#, &[], &[], &["reserve stock"], 0, 8);
    let phrase = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&phrase_request)
            .expect("phrase retrieval succeeds"),
    );
    assert_eq!(phrase.candidates.len(), 1);
    assert_eq!(
        phrase.evidence_by_occurrence[&phrase.candidates[0].source_occurrence_id].matched_phrases,
        vec!["reserve stock".to_owned()]
    );

    let disabled = lexical_request("resreve_stock", &["resreve_stock"], &[], &[], 0, 8);
    assert!(
        complete(
            LexicalLane::new(projection.clone())
                .retrieve_lexical(&disabled)
                .expect("disabled fuzzy retrieval succeeds"),
        )
        .candidates
        .is_empty()
    );

    let fuzzy = lexical_request("resreve_stock", &["resreve_stock"], &[], &[], 1, 8);
    let first = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&fuzzy)
            .expect("fuzzy retrieval succeeds"),
    );
    let second = complete(
        LexicalLane::new(projection)
            .retrieve_lexical(&fuzzy)
            .expect("fuzzy replay succeeds"),
    );
    assert_eq!(first, second);
    assert_eq!(first.candidates.len(), 1);
    assert!(
        first.evidence_by_occurrence[&first.candidates[0].source_occurrence_id]
            .typo_recovery_applied
    );

    let over_budget = lexical_request(
        "resreve_stock",
        &["resreve_stock"],
        &[],
        &[],
        MAX_FUZZY_TERM_EXPANSIONS_V1 + 1,
        8,
    );
    assert!(over_budget.validate().is_err());

    let oversized_term = "x".repeat(MAX_LEXICAL_QUERY_TERM_BYTES_V1 + 1);
    let oversized = lexical_request(&oversized_term, &[oversized_term.as_str()], &[], &[], 1, 8);
    assert!(oversized.validate().is_err());
}

#[test]
fn lexical_phrase_candidate_set_and_frequency_are_reused_without_drift() {
    // Equivalence guard for finding 14: the per-phrase n-gram candidate set is
    // now intersected once and reused for both the document-frequency tally and
    // the lexical document set. Two documents contain the phrase and one does
    // not; the reused candidate set must still return exactly the two
    // phrase-bearing documents, deterministically.
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = vec![
        admitted_rust_chunk(
            &generation,
            1,
            "pub fn reserve() {\n    // reserve stock inventory ledger\n}\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "reserve",
        ),
        admitted_rust_chunk(
            &generation,
            2,
            "pub fn hold() {\n    // reserve stock inventory ledger\n}\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "hold",
        ),
        admitted_rust_chunk(
            &generation,
            3,
            "pub fn unrelated() {\n    // nothing relevant lives here\n}\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "unrelated",
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
    )
    .expect("projection builds");

    let phrase_request = lexical_request(r#""reserve stock""#, &[], &[], &["reserve stock"], 0, 8);
    let first = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&phrase_request)
            .expect("phrase retrieval succeeds"),
    );
    let second = complete(
        LexicalLane::new(projection)
            .retrieve_lexical(&phrase_request)
            .expect("phrase retrieval replays"),
    );

    // Reusing the shared candidate set is deterministic and drift-free.
    assert_eq!(first, second);
    // Exactly the two phrase-bearing documents are returned; the unrelated
    // document is excluded.
    assert_eq!(first.candidates.len(), 2);
    for candidate in &first.candidates {
        assert_eq!(
            first.evidence_by_occurrence[&candidate.source_occurrence_id].matched_phrases,
            vec!["reserve stock".to_owned()]
        );
    }
}

#[test]
fn duplicate_whole_terms_do_not_consume_the_global_fuzzy_budget() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = vec![
        admitted_rust_chunk(
            &generation,
            1,
            "pub fn reserve() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "reserve",
        ),
        admitted_rust_chunk(
            &generation,
            2,
            "pub fn reserved() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "reserved",
        ),
        admitted_rust_chunk(
            &generation,
            3,
            "pub fn other() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "other",
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
    )
    .expect("projection builds");
    let request = lexical_request(
        "reservd reservd otherr",
        &["reservd", "reservd", "otherr"],
        &[],
        &[],
        3,
        8,
    );

    let batch = complete(
        LexicalLane::new(projection)
            .retrieve_lexical(&request)
            .expect("fuzzy retrieval succeeds"),
    );

    assert_eq!(batch.candidates.len(), 3);
    assert!(
        batch
            .evidence_by_occurrence
            .values()
            .any(|evidence| { evidence.matched_whole_terms.contains(&"otherr".to_owned()) })
    );
}

#[test]
fn lexical_projection_reports_freshness_coverage_and_page_cutoff() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks: Vec<ExtractionAdmittedCodeSearchChunkV1> = (1..=3)
        .map(|ordinal| {
            admitted_rust_chunk(
                &generation,
                ordinal,
                "pub fn target() {}\n",
                CodeSearchChunkGrainV1::SymbolSignature,
                "target",
            )
        })
        .collect();
    let current = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks.clone(),
    )
    .expect("current projection builds");
    let request = lexical_request("target", &["target"], &[], &[], 0, 2);

    let page = complete(
        LexicalLane::new(current)
            .retrieve_lexical(&request)
            .expect("page retrieval succeeds"),
    );

    assert_eq!(page.candidates.len(), 2);
    assert_eq!(page.coverage.examined, 3);
    assert_eq!(page.coverage.eligible, 3);
    assert_eq!(page.coverage.capped, 1);
    assert!(!page.continuation.expect("continuation").exhausted);

    let stale = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Stale),
        chunks,
    )
    .expect("stale projection remains inspectable");
    let outcome = LexicalLane::new(stale)
        .retrieve_lexical(&request)
        .expect("staleness is a typed outcome");
    assert!(matches!(outcome, RetrieverOutcome::Stale(_)));
}

#[test]
fn lexical_source_occurrence_identity_is_generation_exact() {
    let first_generation = id::<CodeGenerationId>("generation.1");
    let second_generation = id::<CodeGenerationId>("generation.2");
    let first_projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&first_generation, FreshnessCompatibilityV1::Current),
        vec![admitted_rust_chunk(
            &first_generation,
            1,
            "pub fn target() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "target",
        )],
    )
    .expect("first projection builds");
    let second_projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&second_generation, FreshnessCompatibilityV1::Current),
        vec![admitted_rust_chunk(
            &second_generation,
            1,
            "pub fn target() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "target",
        )],
    )
    .expect("second projection builds");
    let first_request = lexical_request("target", &["target"], &[], &[], 0, 8);
    let mut second_request = lexical_request("target", &["target"], &[], &[], 0, 8);
    second_request.generation = second_generation;

    let first = complete(
        LexicalLane::new(first_projection)
            .retrieve_lexical(&first_request)
            .expect("first retrieval succeeds"),
    );
    let second = complete(
        LexicalLane::new(second_projection)
            .retrieve_lexical(&second_request)
            .expect("second retrieval succeeds"),
    );

    assert_ne!(
        first.candidates[0].anchor_id, second.candidates[0].anchor_id,
        "symbol occurrence anchors are generation-bound"
    );
    assert_ne!(
        first.candidates[0].source_occurrence_id, second.candidates[0].source_occurrence_id,
        "the logical chunk is stable but each generation has a distinct occurrence"
    );
}
