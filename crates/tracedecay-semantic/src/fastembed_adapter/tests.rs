use super::lifecycle_test_support::{
    digest_mismatched_lifecycle_authority, lifecycle_authority_from, lifecycle_install_fixture,
};
use super::*;
use tracedecay_domain::{ChunkerRevision, EmbeddingProjectionKeyV1, PrivacyDomainId};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("canonical test identity")
}

fn digest(byte: char) -> ManifestDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

/// Exact declared bytes of the shipped default code model and its
/// tokenizer, from the production catalog.
const CATALOG_MEMBER_BYTES: u64 = 641_517_466 + 2_561_316;

/// What one session actually costs at the shipped sequence length, stated
/// once so the two guards below cannot drift apart.
///
/// `644,078,782 × 5/4` headroom over declared member bytes, plus the
/// budget-bounded activation of `attention_token_square_budget(32, 4096)`:
/// 1,660,736,493 B, or 1.547 GiB. Measured cold-load resident growth per
/// session on the tiny corpus is 0.96–1.39 GiB, so the reservation is
/// deliberately conservative against what a session retains in practice.
const CATALOG_SESSION_ESTIMATE_BYTES: u64 = 1_660_736_493;

/// The semantic share a 96 GiB host derives (`admitted / 8`), which is
/// what B1 restored to production.
const HOST_DERIVED_CEILING: u64 = 12 * 1024 * 1024 * 1024;

/// A session must be charged what one session retains, not the whole
/// process budget.
///
/// Charging the ceiling per session is self-defeating: the first session
/// reserves the entire budget, so the pool's memory check refuses every
/// later acquisition and embedding collapses to one session on every
/// host, whatever the CPU width arithmetic asked for.
#[test]
fn resident_estimate_admits_more_than_one_session_under_the_process_ceiling() {
    let estimate =
        resident_bytes_estimate_for(CATALOG_MEMBER_BYTES, 32, 4096, HOST_DERIVED_CEILING);
    assert_eq!(estimate, CATALOG_SESSION_ESTIMATE_BYTES);
    assert!(
        estimate >= CATALOG_MEMBER_BYTES,
        "the estimate must still cover the artifact's own declared bytes"
    );
    assert!(
        HOST_DERIVED_CEILING / estimate >= 2,
        "the host-derived ceiling must admit at least the two concurrent \
             sessions the host width arithmetic derives, but only \
             {} fit at {estimate} bytes each",
        HOST_DERIVED_CEILING / estimate
    );
}

/// The shipped 2 GiB default admits exactly one session at the shipped
/// 4096-token sequence. That is the truth, so it is what the test says.
///
/// The reservation is not padded and the ceiling is not raised to make a
/// wider claim pass. Width comes from B1's host derivation reaching
/// production, not from softening this number: an operator who pins
/// 2 GiB is pinning one session, and the doc above says so.
#[test]
fn the_shipped_default_ceiling_admits_exactly_one_session() {
    let estimate = resident_bytes_estimate_for(
        CATALOG_MEMBER_BYTES,
        32,
        4096,
        tracedecay_semantic_contracts::DEFAULT_SEMANTIC_RESIDENT_BYTES,
    );
    assert_eq!(estimate, CATALOG_SESSION_ESTIMATE_BYTES);
    assert_eq!(
        tracedecay_semantic_contracts::DEFAULT_SEMANTIC_RESIDENT_BYTES / estimate,
        1
    );
}

/// End-to-end over the real admission path: a production-scale artifact
/// must not charge one session the whole process budget.
///
/// Before the reservation was split from the ceiling, the descriptor
/// reported the full `max_resident_bytes` for every session, so the
/// pool's `resident_bytes + reserved > memory_ceiling` check refused the
/// second acquisition and `RuntimeChunkVectorEncoderV1::ensure_sessions`
/// silently broke out of its loop at one session.
#[test]
fn production_scale_artifact_admits_the_derived_session_width() {
    const CEILING: u64 = HOST_DERIVED_CEILING;
    const MODEL_BYTES: u64 = 612 * 1024 * 1024;
    const TOKENIZER_BYTES: u64 = 2 * 1024 * 1024;

    let artifact = crate::session_pool::test_support::admitted_artifact_sized(
        MODEL_BYTES,
        TOKENIZER_BYTES,
        CEILING,
    );
    let projection = crate::session_pool::test_support::projection_for(&artifact)
        .admit()
        .expect("production-scale fixture projection");
    let authority = AdmittedProjectionArtifactV1::admit(&artifact, &projection)
        .expect("production-scale fixture admits");

    let reserved = authority.resident_bytes_estimate();
    assert!(
        reserved <= authority.resident_byte_ceiling(),
        "a per-session reservation may never exceed the process ceiling"
    );
    // The pool admits while `resident_bytes + reserved <= ceiling`.
    let admitted = (1..=8)
        .take_while(|n| reserved.saturating_mul(*n) <= CEILING)
        .count();
    assert!(
        admitted >= 2,
        "the host-derived ceiling must admit at least the two concurrent \
             sessions the host width arithmetic derives, but only \
             {admitted} fit at {reserved} bytes each"
    );
}

#[test]
fn resident_estimate_includes_worst_admitted_attention_activations() {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MEMBER_BYTES: u64 = 614 * 1024 * 1024;

    let estimate = resident_bytes_estimate_for(MEMBER_BYTES, 32, 4096, 16 * GIB);
    let member_with_headroom = MEMBER_BYTES * 5 / 4;
    assert_eq!(
        estimate,
        member_with_headroom + fastembed_worst_batch_activation_bytes(32, 4096)
    );
}

fn authority(dimensions: u32) -> AdmittedProjectionArtifactV1 {
    authority_with(
        dimensions,
        'a',
        EmbeddingMetricV1::Cosine,
        EmbeddingNormalizationV1::L2,
    )
}

fn authority_with(
    dimensions: u32,
    artifact_digest: char,
    metric: EmbeddingMetricV1,
    normalization: EmbeddingNormalizationV1,
) -> AdmittedProjectionArtifactV1 {
    let projection = EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest(artifact_digest),
        tokenizer_digest: digest('b'),
        config_digest: digest('c'),
        query_instruction_digest: Some(digest('d')),
        document_instruction_digest: Some(digest('e')),
        document_composition: EmbeddingDocumentCompositionV1::SanitizedText,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 4096,
        inference_batch_size: 8,
        inference_batch_bytes: 16 * 1024,
        runtime_backend: "fastembed-ort".to_owned(),
        runtime_build_revision: "ort-test-rev-1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        execution_provider: EmbeddingExecutionProviderV1::Cpu,
        dimensions,
        metric,
        normalization,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.test"),
        privacy_key_epoch: 7,
    }
    .admit()
    .expect("valid test projection");
    AdmittedProjectionArtifactV1 {
        runtime_artifact: VerifiedEmbeddingArtifactV1 {
            projection,
            backend: EmbeddingRuntimeFamilyV1::FastEmbedOrt,
            model_file: "model.onnx".to_string(),
            tokenizer_file: "tokenizer.json".to_string(),
            config_file: "config.json".to_string(),
            artifact: None,
            lifecycle_install: None,
            max_batch_texts: 8,
            max_batch_bytes: 16 * 1024,
            max_threads: 4,
            max_concurrent_sessions: 4,
            resident_byte_ceiling: 64 * 1024 * 1024,
            resident_bytes_estimate: 8 * 1024 * 1024,
            load_deadline_ms: 30_000,
        },
    }
}

fn batch(texts: &[&str]) -> BoundedSanitizedTextBatchV1 {
    BoundedSanitizedTextBatchV1::try_new(
        texts.iter().map(|t| (*t).to_string()).collect(),
        64,
        1 << 20,
    )
    .expect("batch within bounds")
}

fn never_cancelled() -> ManualCancellation {
    ManualCancellation::new()
}

#[test]
fn lifecycle_install_authority_verifies_member_bytes_at_read() {
    let fixture = lifecycle_install_fixture(b"model");
    let authority = lifecycle_authority_from(&fixture, 1024).expect("verified lifecycle authority");

    assert_eq!(
        authority
            .runtime_artifact()
            .required_member_bytes(ArtifactMemberRoleV1::Model)
            .expect("model bytes"),
        b"model"
    );
    #[cfg(all(feature = "semantic-fastembed", not(windows)))]
    {
        let runtime = FastEmbedEmbeddingRuntime;
        runtime
            .verify_artifact_compatibility(&authority)
            .expect("production runtime admits the verified lifecycle install");
        assert!(matches!(
            runtime.open_session(&authority, &never_cancelled()),
            Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind: RuntimeFailureKindV1::LoadFailed,
                ..
            }))
        ));
    }
    std::fs::write(fixture.install.path().join("tokenizer.json"), b"mutated")
        .expect("corrupt tokenizer");
    assert!(matches!(
        authority
            .runtime_artifact()
            .required_member_bytes(ArtifactMemberRoleV1::Tokenizer),
        Err(EmbedError::Runtime(_))
    ));
}

#[test]
fn lifecycle_authority_construction_reads_no_member_bytes() {
    // The fixture's model member has the pinned length but not the
    // pinned digest. Only reading and hashing the file could detect the
    // mismatch, so the successful construction inside the fixture
    // helper proves zero member byte reads at construction.
    let mismatched = digest_mismatched_lifecycle_authority();
    let authority = mismatched.authority;

    assert!(
        matches!(
            authority
                .runtime_artifact()
                .required_member_bytes(ArtifactMemberRoleV1::Model),
            Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind: RuntimeFailureKindV1::CorruptArtifact,
                ..
            }))
        ),
        "every byte consumption still verifies the digest pin"
    );
    assert!(
        matches!(
            FakeEmbeddingRuntime::new().open_session(&authority, &never_cancelled()),
            Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind: RuntimeFailureKindV1::CorruptArtifact,
                ..
            }))
        ),
        "the default-runtime session open also rejects digest-mismatched bytes"
    );
    #[cfg(all(feature = "semantic-fastembed", not(windows)))]
    assert!(
        matches!(
            FastEmbedEmbeddingRuntime.open_session(&authority, &never_cancelled()),
            Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind: RuntimeFailureKindV1::CorruptArtifact,
                ..
            }))
        ),
        "no session can open over digest-mismatched member bytes"
    );
}

#[test]
fn open_session_honors_interruption_before_member_bytes() {
    // The fixture's member bytes are digest-mismatched, so any byte read
    // would fail with CorruptArtifact. A fired interruption must win
    // instead: the typed Cancelled proves the stage-boundary check runs
    // before the first member read.
    let cancelled = ManualCancellation::new();
    cancelled.cancel();
    let mismatched = digest_mismatched_lifecycle_authority();
    let authority = mismatched.authority;

    assert!(
        matches!(
            FakeEmbeddingRuntime::new().open_session(&authority, &cancelled),
            Err(EmbedError::Cancelled)
        ),
        "cancellation must be observed before any member byte is read"
    );
    #[cfg(all(feature = "semantic-fastembed", not(windows)))]
    assert!(
        matches!(
            FastEmbedEmbeddingRuntime.open_session(&authority, &cancelled),
            Err(EmbedError::Cancelled)
        ),
        "the production adapter must abandon the open before buffering members"
    );
}

#[cfg(all(feature = "semantic-fastembed", not(windows)))]
#[test]
fn fastembed_errors_classify_by_typed_variant_with_a_fixed_detail() {
    use RuntimeFailureKindV1::{EmbedFailed, IncompatibleRuntime, LoadFailed, OutOfMemory};

    let classify = |fallback, error: fastembed::Error| match super::fastembed_error(
        fallback,
        "stage detail",
        &error,
    ) {
        EmbedError::Runtime(failure) => {
            assert_eq!(
                failure.detail, "stage detail",
                "raw runtime text must never replace the stage detail"
            );
            failure.kind
        }
        other => panic!("expected a runtime failure, got {other:?}"),
    };

    // ORT failures keep the stage's own kind unless memory ran out.
    assert_eq!(
        classify(
            LoadFailed,
            fastembed::Error::OrtBuilder("bad option".into())
        ),
        LoadFailed
    );
    assert_eq!(
        classify(
            EmbedFailed,
            fastembed::Error::OrtSession("shape mismatch".into())
        ),
        EmbedFailed
    );
    assert_eq!(
        classify(
            LoadFailed,
            fastembed::Error::OrtSession("Failed to allocate memory: out of memory".into())
        ),
        OutOfMemory
    );
    assert_eq!(
        classify(
            EmbedFailed,
            fastembed::Error::OrtSession("bad allocation".into())
        ),
        OutOfMemory
    );
    // Digest-verified tokenizer members the linked runtime rejects.
    assert_eq!(
        classify(
            LoadFailed,
            fastembed::Error::TokenizerConfig("missing pad_token".into())
        ),
        IncompatibleRuntime
    );
    // Input-side tokenizer failures are embedding failures.
    assert_eq!(
        classify(LoadFailed, fastembed::Error::Tokenization("encode".into())),
        EmbedFailed
    );
    assert_eq!(
        classify(EmbedFailed, fastembed::Error::EmptyTokenizations),
        EmbedFailed
    );
    // Everything else (including variants added later) keeps the fallback.
    assert_eq!(
        classify(LoadFailed, fastembed::Error::Other("unknown".into())),
        LoadFailed
    );
    assert_eq!(
        classify(
            EmbedFailed,
            fastembed::Error::InvalidArgument("batch".into())
        ),
        EmbedFailed
    );
}

#[test]
fn lifecycle_authority_construction_rejects_structural_pin_violations() {
    let fixture = lifecycle_install_fixture(b"model");
    let model_path = fixture.install.path().join("model.onnx");

    std::fs::write(&model_path, b"model-longer-than-pin").expect("length-mismatched member");
    assert!(
        matches!(
            lifecycle_authority_from(&fixture, 1024),
            Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind: RuntimeFailureKindV1::CorruptArtifact,
                ..
            }))
        ),
        "a length-pin mismatch fails construction eagerly"
    );

    std::fs::remove_file(&model_path).expect("remove model member");
    assert!(
        matches!(
            lifecycle_authority_from(&fixture, 1024),
            Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind: RuntimeFailureKindV1::CorruptArtifact,
                ..
            }))
        ),
        "a missing member fails construction eagerly"
    );

    #[cfg(unix)]
    {
        std::fs::write(fixture.install.path().join("model.real"), b"model")
            .expect("symlink target");
        std::os::unix::fs::symlink(fixture.install.path().join("model.real"), &model_path)
            .expect("symlinked member");
        assert!(
            matches!(
                lifecycle_authority_from(&fixture, 1024),
                Err(EmbedError::Runtime(RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::CorruptArtifact,
                    ..
                }))
            ),
            "a symlinked member fails construction eagerly"
        );
    }
}

/// The removed construction work is exactly one [`read_member_bytes`]
/// pass over every member — still the per-session-open verification —
/// so construction must do strictly less: it may stat member pins but
/// never open one for reading. Proven by operations, not wall clocks:
/// the verification pass must return every member's exact pinned bytes
/// (impossible without reading all of them), while construction still
/// succeeds after member read permission is revoked (any
/// construction-time byte read would fail the constructor and this
/// test).
#[test]
fn lifecycle_authority_construction_is_cheaper_than_member_byte_verification() {
    let fixture = lifecycle_install_fixture(b"model");
    let authority = lifecycle_authority_from(&fixture, 1024).expect("verified lifecycle authority");
    for (role, pinned) in [
        (ArtifactMemberRoleV1::Model, b"model".as_slice()),
        (ArtifactMemberRoleV1::Tokenizer, b"tokenizer".as_slice()),
        (ArtifactMemberRoleV1::Config, b"config".as_slice()),
        (
            ArtifactMemberRoleV1::SpecialTokensMap,
            b"special".as_slice(),
        ),
        (
            ArtifactMemberRoleV1::TokenizerConfig,
            b"tokenizer-config".as_slice(),
        ),
    ] {
        assert_eq!(
            authority
                .runtime_artifact()
                .required_member_bytes(role)
                .expect("baseline member byte verification"),
            pinned,
            "one verification pass must consume every member's bytes"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for member in [
            "model.onnx",
            "tokenizer.json",
            "config.json",
            "special_tokens_map.json",
            "tokenizer_config.json",
        ] {
            std::fs::set_permissions(
                fixture.install.path().join(member),
                std::fs::Permissions::from_mode(0o000),
            )
            .expect("revoke member read permission");
        }
        assert!(
            std::fs::read(fixture.install.path().join("model.onnx")).is_err(),
            "the fixture requires a test runner whose member reads are deniable"
        );
        let unreadable = lifecycle_authority_from(&fixture, 1024)
            .expect("construction checks structural pins without opening member bytes for reading");
        assert!(
            matches!(
                unreadable
                    .runtime_artifact()
                    .required_member_bytes(ArtifactMemberRoleV1::Model),
                Err(EmbedError::Runtime(RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::CorruptArtifact,
                    ..
                }))
            ),
            "the byte-verification pass cannot succeed without reading, so the revocation \
                 that construction tolerates provably blocks the read path"
        );
    }
}

#[test]
fn batch_constructor_enforces_bounds() {
    assert!(matches!(
        BoundedSanitizedTextBatchV1::try_new(vec![], 4, 16),
        Err(EmbedError::EmptyBatch)
    ));
    assert!(matches!(
        BoundedSanitizedTextBatchV1::try_new(vec!["a".to_string(), "b".to_string()], 1, 16),
        Err(EmbedError::TooManyTexts {
            presented: 2,
            max: 1
        })
    ));
    assert!(matches!(
        BoundedSanitizedTextBatchV1::try_new(vec!["abcdef".to_string()], 4, 3),
        Err(EmbedError::BatchBytesExceeded {
            presented: 6,
            max: 3
        })
    ));
}

#[test]
fn cancellation_before_embed_aborts() {
    let runtime = FakeEmbeddingRuntime::new();
    let mut session = runtime
        .open_session(&authority(8), &never_cancelled())
        .expect("session");
    let cancel = ManualCancellation::new();
    cancel.cancel();
    let result = session.embed_batch(&batch(&["a", "b"]), &cancel);
    assert!(matches!(result, Err(EmbedError::Cancelled)));
    assert_eq!(
        runtime.counters().texts_embedded.load(Ordering::SeqCst),
        0,
        "no text embedded after pre-cancel"
    );
}

#[test]
fn deadline_before_embed_surfaces_typed_expiry_without_inference() {
    struct ExpiredAuthority;

    impl SemanticExecutionAuthority for ExpiredAuthority {
        fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
            Some(SemanticExecutionInterruptionV1::DeadlineExceeded)
        }
    }

    let runtime = FakeEmbeddingRuntime::new();
    let mut session = runtime
        .open_session(&authority(8), &never_cancelled())
        .expect("session");
    let result = session.embed_batch(&batch(&["a", "b"]), &ExpiredAuthority);

    assert_eq!(result, Err(EmbedError::DeadlineExceeded));
    assert_eq!(
        runtime.counters().texts_embedded.load(Ordering::SeqCst),
        0,
        "expired work must not enter inference"
    );
}

#[test]
fn cancellation_mid_embed_discards_partial_batch() {
    let runtime = FakeEmbeddingRuntime::new();
    let mut session = runtime
        .open_session(&authority(8), &never_cancelled())
        .expect("session");
    // First poll (before text 1) passes, second poll cancels.
    let cancel = ScriptedCancellation::new(1);
    let result = session.embed_batch(&batch(&["a", "b", "c", "d"]), &cancel);
    assert!(matches!(result, Err(EmbedError::Cancelled)));
    assert_eq!(
        runtime.counters().texts_embedded.load(Ordering::SeqCst),
        1,
        "exactly one text embedded before cancellation; no partial batch returned"
    );
}

#[test]
fn session_enforces_its_own_manifest_batch_ceiling() {
    let runtime = FakeEmbeddingRuntime::new();
    let mut authority = authority(8);
    authority.runtime_artifact.max_batch_texts = 1;
    let mut session = runtime
        .open_session(&authority, &never_cancelled())
        .expect("session");
    let result = session.embed_batch(&batch(&["a", "b"]), &never_cancelled());
    assert!(matches!(
        result,
        Err(EmbedError::TooManyTexts {
            presented: 2,
            max: 1
        })
    ));
}

#[test]
fn vector_validation_rejects_bad_shape_and_nonfinite_values() {
    let mut v = EmbeddingVectorV1 {
        values: vec![0.0; 3],
        dimensions: 4,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
    };
    assert!(matches!(
        v.validate(),
        Err(EmbedError::DimensionMismatch {
            expected: 4,
            actual: 3
        })
    ));
    v.dimensions = 3;
    v.values[1] = f32::NAN;
    assert!(matches!(
        v.validate(),
        Err(EmbedError::NonFiniteVectorValue)
    ));
    v.values[1] = f32::INFINITY;
    assert!(matches!(
        v.validate(),
        Err(EmbedError::NonFiniteVectorValue)
    ));
}

#[test]
fn open_failure_is_typed_and_disables_nothing_silently() {
    for kind in [
        RuntimeFailureKindV1::OutOfMemory,
        RuntimeFailureKindV1::CorruptArtifact,
        RuntimeFailureKindV1::RevokedArtifact,
        RuntimeFailureKindV1::IncompatibleRuntime,
        RuntimeFailureKindV1::LoadFailed,
        RuntimeFailureKindV1::EmbedFailed,
    ] {
        let runtime = FakeEmbeddingRuntime::new().with_open_failure(kind);
        let result = runtime.open_session(&authority(8), &never_cancelled());
        match result {
            Err(EmbedError::Runtime(failure)) => assert_eq!(failure.kind, kind),
            other => panic!("expected typed runtime failure, got {other:?}"),
        }
    }
}

#[test]
fn compatibility_failure_is_typed() {
    let runtime = FakeEmbeddingRuntime::new()
        .with_compatibility_failure(RuntimeFailureKindV1::IncompatibleRuntime);
    let result = runtime.verify_artifact_compatibility(&authority(8));
    match result {
        Err(EmbedError::Runtime(failure)) => {
            assert_eq!(failure.kind, RuntimeFailureKindV1::IncompatibleRuntime);
        }
        other => panic!("expected typed compatibility failure, got {other:?}"),
    }
}

#[cfg(all(feature = "semantic-fastembed", not(windows)))]
#[test]
fn real_fastembed_runtime_rejects_unnormalized_projection_before_loading() {
    let runtime = FastEmbedEmbeddingRuntime;
    let result = runtime.verify_artifact_compatibility(&authority_with(
        8,
        'a',
        EmbeddingMetricV1::Cosine,
        EmbeddingNormalizationV1::None,
    ));
    assert!(matches!(
        result,
        Err(EmbedError::Runtime(RuntimeFailureV1 {
            kind: RuntimeFailureKindV1::IncompatibleRuntime,
            ..
        }))
    ));
}
