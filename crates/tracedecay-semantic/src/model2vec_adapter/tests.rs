//! Known-answer and failure-path tests for the Model2Vec static runtime over
//! the synthetic fixture package in
//! `fastembed_adapter::lifecycle_test_support`. Nothing here downloads a
//! model or reads a live profile.
use tracedecay_domain::{
    EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
};

use crate::embedding_backend::{EmbeddingRuntimeFamilyV1, ProductionEmbeddingRuntime};
use crate::fastembed_adapter::lifecycle_test_support::{
    LifecycleInstallFixtureV1, MODEL2VEC_FIXTURE_TOKENIZER_JSON, lifecycle_authority_from,
    model2vec_fixture_table_bytes, model2vec_lifecycle_install_fixture,
};
use crate::fastembed_adapter::{
    AdmittedProjectionArtifactV1, BoundedSanitizedTextBatchV1, EmbedError, EmbeddingRuntime,
    EmbeddingSession, ManualCancellation, RuntimeFailureKindV1, ScriptedCancellation,
};

use super::{
    MODEL2VEC_RUNTIME_BUILD_REVISION_V1, MODEL2VEC_RUNTIME_FAMILY_V1, Model2VecEmbeddingRuntime,
    StaticEmbeddingModelV1, TokenizedTextV1,
};

const NORMALIZED_CONFIG: &str = r#"{"normalize": true, "embedding_dtype": "float16"}"#;

fn assert_close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "dimension");
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (actual - expected).abs() < 1e-6,
            "component {index}: {actual} != {expected}"
        );
    }
}

fn load_fixture_model(
    precision: EmbeddingPrecisionV1,
    truncation_length: usize,
) -> StaticEmbeddingModelV1 {
    StaticEmbeddingModelV1::load(
        NORMALIZED_CONFIG.as_bytes(),
        MODEL2VEC_FIXTURE_TOKENIZER_JSON.as_bytes(),
        &model2vec_fixture_table_bytes(precision),
        3,
        precision,
        truncation_length,
    )
    .expect("fixture model loads")
}

fn runtime_failure(error: EmbedError) -> (RuntimeFailureKindV1, String) {
    match error {
        EmbedError::Runtime(failure) => (failure.kind, failure.detail),
        other => panic!("expected a runtime failure, got {other:?}"),
    }
}

#[test]
fn known_answer_vectors_are_mean_pooled_and_l2_normalized() {
    let model = load_fixture_model(EmbeddingPrecisionV1::Fp16, 8);
    let (hello, truncated) = model.embed("hello").expect("hello");
    assert!(!truncated);
    assert_close(&hello, &[1.0, 0.0, 0.0]);

    // mean([1,0,0],[0,2,0]) = [0.5,1,0]; /sqrt(1.25)
    let (hello_world, truncated) = model.embed("hello world").expect("hello world");
    assert!(!truncated);
    let norm = 1.25_f32.sqrt();
    assert_close(&hello_world, &[0.5 / norm, 1.0 / norm, 0.0]);

    // mean of all three rows = [1/3, 2/3, 4/3]; normalized.
    let (all, _) = model.embed("Hello World CODE").expect("lowercased");
    let raw = [1.0_f32 / 3.0, 2.0 / 3.0, 4.0 / 3.0];
    let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert_close(&all, &[raw[0] / norm, raw[1] / norm, raw[2] / norm]);
    let squared: f32 = all.iter().map(|v| v * v).sum();
    assert!((squared - 1.0).abs() < 1e-5, "unit norm, got {squared}");
}

#[test]
fn f32_tables_produce_the_same_vectors_as_f16_tables() {
    let f16 = load_fixture_model(EmbeddingPrecisionV1::Fp16, 8);
    let f32 = load_fixture_model(EmbeddingPrecisionV1::Fp32, 8);
    for text in ["hello", "hello world", "code world hello", "world"] {
        let (a, _) = f16.embed(text).expect("f16");
        let (b, _) = f32.embed(text).expect("f32");
        assert_close(&a, &b);
    }
}

#[test]
fn unknown_tokens_are_dropped_before_pooling() {
    let model = load_fixture_model(EmbeddingPrecisionV1::Fp16, 8);
    assert_eq!(
        model.tokenize("hello zzz world").expect("tokens"),
        TokenizedTextV1 {
            ids: vec![1, 2],
            truncated: false,
        }
    );
    let (with_unknown, _) = model.embed("hello zzz world").expect("with unknown");
    let (without, _) = model.embed("hello world").expect("without");
    assert_close(&with_unknown, &without);
    // The [UNK] row is [100,100,100]; had it leaked in, the vector would
    // be dominated by it and nearly uniform.
    assert!(with_unknown[2].abs() < 1e-6);
}

#[test]
fn texts_without_known_tokens_yield_the_zero_vector() {
    let model = load_fixture_model(EmbeddingPrecisionV1::Fp16, 8);
    for text in ["", "   ", "zzz qqq", "!!!"] {
        let (vector, truncated) = model.embed(text).expect("empty pooling");
        assert!(!truncated);
        assert_eq!(vector, vec![0.0, 0.0, 0.0], "text {text:?}");
        assert!(vector.iter().all(|value| value.is_sign_positive()));
    }
}

#[test]
fn truncation_follows_the_projection_not_the_serialized_tokenizer() {
    // The fixture tokenizer serializes `max_length = 2`; a projection with
    // budget 8 must still see all three tokens.
    let wide = load_fixture_model(EmbeddingPrecisionV1::Fp16, 8);
    assert_eq!(
        wide.tokenize("hello world code").expect("tokens"),
        TokenizedTextV1 {
            ids: vec![1, 2, 3],
            truncated: false,
        }
    );
    let narrow = load_fixture_model(EmbeddingPrecisionV1::Fp16, 2);
    let tokens = narrow.tokenize("hello world code").expect("tokens");
    assert_eq!(
        tokens,
        TokenizedTextV1 {
            ids: vec![1, 2],
            truncated: true,
        }
    );
    // Unknown tokens are removed before the budget applies, matching the
    // upstream reference: `hello zzz world code` keeps hello+world.
    assert_eq!(
        narrow.tokenize("hello zzz world code").expect("tokens").ids,
        vec![1, 2]
    );
    let (truncated_vector, truncated) = narrow.embed("hello world code").expect("truncated");
    assert!(truncated);
    let (pair, _) = narrow.embed("hello world").expect("pair");
    assert_close(&truncated_vector, &pair);
}

#[test]
fn token_ids_outside_the_table_are_a_typed_corrupt_artifact() {
    let model = load_fixture_model(EmbeddingPrecisionV1::Fp16, 8);
    let error = model
        .embed("hello ghost")
        .expect_err("ghost has no table row");
    let (kind, detail) = runtime_failure(error);
    assert_eq!(kind, RuntimeFailureKindV1::CorruptArtifact);
    assert!(detail.contains("outside the embedding table"), "{detail}");
    assert_eq!(
        runtime_failure(model.pool(&[u32::MAX]).expect_err("overflowing id")).0,
        RuntimeFailureKindV1::CorruptArtifact
    );
}

#[test]
fn table_decode_rejects_precision_shape_and_config_mismatches() {
    let load = |config: &str, table: &[u8], dimension: usize, precision: EmbeddingPrecisionV1| {
        StaticEmbeddingModelV1::load(
            config.as_bytes(),
            MODEL2VEC_FIXTURE_TOKENIZER_JSON.as_bytes(),
            table,
            dimension,
            precision,
            8,
        )
        .map(|_| ())
    };
    let f16_table = model2vec_fixture_table_bytes(EmbeddingPrecisionV1::Fp16);
    let f32_table = model2vec_fixture_table_bytes(EmbeddingPrecisionV1::Fp32);

    let (kind, detail) = runtime_failure(
        load(NORMALIZED_CONFIG, &f32_table, 3, EmbeddingPrecisionV1::Fp16).expect_err("dtype"),
    );
    assert_eq!(kind, RuntimeFailureKindV1::CorruptArtifact);
    assert!(detail.contains("dtype"), "{detail}");

    let (kind, detail) = runtime_failure(
        load(
            NORMALIZED_CONFIG,
            &f16_table,
            256,
            EmbeddingPrecisionV1::Fp16,
        )
        .expect_err("shape"),
    );
    assert_eq!(kind, RuntimeFailureKindV1::CorruptArtifact);
    assert!(detail.contains("dimensions"), "{detail}");

    let (kind, _) = runtime_failure(
        load(
            NORMALIZED_CONFIG,
            b"not safetensors",
            3,
            EmbeddingPrecisionV1::Fp16,
        )
        .expect_err("garbage"),
    );
    assert_eq!(kind, RuntimeFailureKindV1::CorruptArtifact);

    let (kind, detail) = runtime_failure(
        load(
            r#"{"normalize": false}"#,
            &f16_table,
            3,
            EmbeddingPrecisionV1::Fp16,
        )
        .expect_err("unnormalized"),
    );
    assert_eq!(kind, RuntimeFailureKindV1::IncompatibleRuntime);
    assert!(detail.contains("normalization"), "{detail}");

    let (kind, _) = runtime_failure(
        load(
            r#"{"dtype": "float16"}"#,
            &f16_table,
            3,
            EmbeddingPrecisionV1::Fp16,
        )
        .expect_err("missing normalize"),
    );
    assert_eq!(kind, RuntimeFailureKindV1::CorruptArtifact);

    assert!(load(NORMALIZED_CONFIG, &f16_table, 3, EmbeddingPrecisionV1::Fp16).is_ok());
    assert!(load(NORMALIZED_CONFIG, &f32_table, 3, EmbeddingPrecisionV1::Fp32).is_ok());
}

fn fixture_authority(fixture: &LifecycleInstallFixtureV1) -> AdmittedProjectionArtifactV1 {
    lifecycle_authority_from(fixture, 4096).expect("fixture authority")
}

fn batch(texts: &[&str]) -> BoundedSanitizedTextBatchV1 {
    BoundedSanitizedTextBatchV1::try_new(
        texts.iter().map(|text| (*text).to_owned()).collect(),
        4,
        4 * 128 * 4,
    )
    .expect("bounded batch")
}

#[test]
fn lifecycle_install_opens_a_session_and_embeds_typed_vectors() {
    let fixture =
        model2vec_lifecycle_install_fixture(EmbeddingPrecisionV1::Fp16, NORMALIZED_CONFIG, 8);
    let authority = fixture_authority(&fixture);
    let key: &EmbeddingProjectionKeyV1 = authority.projection().embedding_key();
    assert_eq!(key.runtime_backend, MODEL2VEC_RUNTIME_FAMILY_V1);
    assert_eq!(
        key.runtime_build_revision,
        MODEL2VEC_RUNTIME_BUILD_REVISION_V1
    );
    assert_eq!(key.precision, EmbeddingPrecisionV1::Fp16);
    assert_eq!(key.dimensions, 3);
    assert_eq!(key.truncation_length, 8);
    assert_eq!(
        authority.runtime_family(),
        EmbeddingRuntimeFamilyV1::Model2VecStatic
    );

    let runtime = Model2VecEmbeddingRuntime;
    runtime
        .verify_artifact_compatibility(&authority)
        .expect("compatible");
    let cancellation = ManualCancellation::new();
    let mut session = runtime
        .open_session(&authority, &cancellation)
        .expect("session opens");
    assert_eq!(session.authority(), &authority);
    assert_eq!(
        session.resident_bytes_estimate(),
        authority.resident_bytes_estimate()
    );

    let vectors = session
        .embed_batch(&batch(&["hello", "zzz", "hello world"]), &cancellation)
        .expect("embed");
    assert_eq!(vectors.len(), 3);
    for vector in &vectors {
        assert_eq!(vector.dimensions, 3);
        assert_eq!(vector.metric, EmbeddingMetricV1::Cosine);
        assert_eq!(vector.normalization, EmbeddingNormalizationV1::L2);
        vector.validate().expect("finite, right dimension");
    }
    assert_close(&vectors[0].values, &[1.0, 0.0, 0.0]);
    assert_eq!(vectors[1].values, vec![0.0, 0.0, 0.0]);
    let norm = 1.25_f32.sqrt();
    assert_close(&vectors[2].values, &[0.5 / norm, 1.0 / norm, 0.0]);
}

#[test]
fn production_dispatcher_routes_a_model2vec_authority_to_the_static_runtime() {
    let fixture =
        model2vec_lifecycle_install_fixture(EmbeddingPrecisionV1::Fp32, NORMALIZED_CONFIG, 8);
    let authority = fixture_authority(&fixture);
    let runtime = ProductionEmbeddingRuntime::default();
    runtime
        .verify_artifact_compatibility(&authority)
        .expect("dispatched compatibility");
    let cancellation = ManualCancellation::new();
    let mut session = runtime
        .open_session(&authority, &cancellation)
        .expect("dispatched session");
    assert!(matches!(
        session,
        crate::embedding_backend::ProductionEmbeddingSession::Model2Vec(_)
    ));
    let vectors = session
        .embed_batch(&batch(&["code"]), &cancellation)
        .expect("dispatched embed");
    assert_close(&vectors[0].values, &[0.0, 0.0, 1.0]);
}

#[test]
fn open_session_honors_cancellation_between_member_reads() {
    let fixture =
        model2vec_lifecycle_install_fixture(EmbeddingPrecisionV1::Fp16, NORMALIZED_CONFIG, 8);
    let authority = fixture_authority(&fixture);
    let runtime = Model2VecEmbeddingRuntime;

    let immediate = ManualCancellation::new();
    immediate.cancel();
    assert_eq!(
        runtime.open_session(&authority, &immediate).err(),
        Some(EmbedError::Cancelled)
    );

    // Poll 1: entry. Polls 2-4: before each member read. Poll 5: before the
    // decode. Every boundary must surface as `Cancelled`, never as a
    // half-built session.
    let boundaries = {
        let counter = ScriptedCancellation::new(usize::MAX);
        let _ = runtime.open_session(&authority, &counter);
        counter.polls()
    };
    assert!(
        boundaries >= 4,
        "expected several boundaries, saw {boundaries}"
    );
    for cancel_after in 0..boundaries {
        let scripted = ScriptedCancellation::new(cancel_after);
        assert_eq!(
            runtime.open_session(&authority, &scripted).err(),
            Some(EmbedError::Cancelled),
            "cancel_after {cancel_after}"
        );
    }
}

#[test]
fn embed_batch_honors_cancellation_between_texts_without_partial_output() {
    let fixture =
        model2vec_lifecycle_install_fixture(EmbeddingPrecisionV1::Fp16, NORMALIZED_CONFIG, 8);
    let authority = fixture_authority(&fixture);
    let runtime = Model2VecEmbeddingRuntime;
    let mut session = runtime
        .open_session(&authority, &ManualCancellation::new())
        .expect("session");

    // Text 1 passes its check, text 2 observes the cancellation.
    let scripted = ScriptedCancellation::new(1);
    assert_eq!(
        session
            .embed_batch(&batch(&["hello", "world", "code"]), &scripted)
            .err(),
        Some(EmbedError::Cancelled)
    );
    // The session remains usable afterwards.
    let vectors = session
        .embed_batch(&batch(&["world"]), &ManualCancellation::new())
        .expect("session survives cancellation");
    assert_close(&vectors[0].values, &[0.0, 1.0, 0.0]);
}

#[test]
fn incompatible_projection_pins_are_rejected_before_any_member_read() {
    let fixture = model2vec_lifecycle_install_fixture(
        EmbeddingPrecisionV1::Fp16,
        r#"{"normalize": false}"#,
        8,
    );
    let authority = fixture_authority(&fixture);
    let runtime = Model2VecEmbeddingRuntime;
    // Compatibility inspects only the admitted descriptor; the disabled
    // normalization is caught at load, typed.
    runtime
        .verify_artifact_compatibility(&authority)
        .expect("descriptor is compatible");
    let (kind, _) = runtime_failure(
        runtime
            .open_session(&authority, &ManualCancellation::new())
            .err()
            .expect("unnormalized config"),
    );
    assert_eq!(kind, RuntimeFailureKindV1::IncompatibleRuntime);

    // A same-length, digest-mismatched table fails typed at open.
    let fixture =
        model2vec_lifecycle_install_fixture(EmbeddingPrecisionV1::Fp16, NORMALIZED_CONFIG, 8);
    let authority = fixture_authority(&fixture);
    let mut corrupted = model2vec_fixture_table_bytes(EmbeddingPrecisionV1::Fp16);
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0x01;
    std::fs::write(fixture.install.path().join("model.safetensors"), corrupted)
        .expect("corrupt table");
    let (kind, _) = runtime_failure(
        runtime
            .open_session(&authority, &ManualCancellation::new())
            .err()
            .expect("digest mismatch"),
    );
    assert_eq!(kind, RuntimeFailureKindV1::CorruptArtifact);
}
