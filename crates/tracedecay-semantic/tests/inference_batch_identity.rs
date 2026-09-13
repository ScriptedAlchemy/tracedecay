//! Byte-identity evidence for the cataloged Jina model when token-length
//! bucketing changes which texts share a FastEmbed forward pass.
//!
//! Groups under-fill badly on real corpora — every multi-chunk file flushes
//! its own partial group — so merging them is the obvious throughput win. It
//! is not available, and this harness is why.
//!
//! FastEmbed's `TextEmbedding::transform` splits its input with
//! `texts.chunks(batch_size)` and runs one `ort::Session::run` per chunk. The
//! tokenizer pads with `PaddingStrategy::BatchLongest`, so regrouping changes
//! the padded input shape. Vector bytes feed `vector_output_digest`; this gate
//! therefore requires the only production FastEmbed model to remain exactly
//! byte-identical under the new grouping.
//!
//! Gated like every other model-dependent test in this crate: `#[ignore]` by
//! default, and a hard failure — never a silent skip — when it is run without
//! the verified fixture it needs.
//!
//! ```text
//! TRACEDECAY_FASTEMBED_FIXTURE_DIR=<dir> \
//!   cargo test -p tracedecay-semantic --features semantic-fastembed \
//!   --test inference_batch_identity -- --ignored --nocapture
//! ```
#![cfg(all(feature = "semantic-fastembed", not(windows)))]
#![forbid(unsafe_code)]

use std::{env, fs, path::Path};

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

const FIXTURE_ENV: &str = "TRACEDECAY_FASTEMBED_FIXTURE_DIR";

/// `SemanticResourceCeilings::default()`: 32 texts, 4096 tokens.
const PRODUCTION_BATCH_SIZE: usize = 32;
const PRODUCTION_SEQUENCE_LENGTH: usize = 4096;
/// `embedding_parallelism::DEFAULT_INTRA_THREADS`.
const PRODUCTION_INTRA_THREADS: usize = 4;

fn read(root: &Path, name: &str) -> Vec<u8> {
    fs::read(root.join(name)).unwrap_or_else(|error| {
        panic!(
            "cannot read FastEmbed fixture member {}: {error}",
            root.join(name).display()
        )
    })
}

fn model(root: &Path, intra_threads: usize) -> TextEmbedding {
    let defined = UserDefinedEmbeddingModel::new(
        read(root, "model.onnx"),
        TokenizerFiles {
            tokenizer_file: read(root, "tokenizer.json"),
            config_file: read(root, "config.json"),
            special_tokens_map_file: read(root, "special_tokens_map.json"),
            tokenizer_config_file: read(root, "tokenizer_config.json"),
        },
    )
    .with_pooling(Pooling::Mean);
    TextEmbedding::try_new_from_user_defined(
        defined,
        InitOptionsUserDefined::new()
            .with_max_length(PRODUCTION_SEQUENCE_LENGTH)
            .with_intra_threads(intra_threads),
    )
    .expect("the local fixture must initialize bundled ORT")
}

/// Differing IEEE lanes and the worst absolute gap between two vector sets.
fn compare(left: &[Vec<f32>], right: &[Vec<f32>]) -> (usize, f32) {
    let mut differing = 0usize;
    let mut worst = 0.0f32;
    for (expected, actual) in left.iter().zip(right.iter()) {
        for (a, b) in expected.iter().zip(actual.iter()) {
            if a.to_bits() != b.to_bits() {
                differing += 1;
            }
            worst = worst.max((a - b).abs());
        }
    }
    (differing, worst)
}

/// A short chunk, the shape a multi-chunk file's remainder group is made of.
fn short_chunk(index: usize) -> String {
    format!("fn accessor_{index}(&self) -> u32 {{ self.field_{index} }}")
}

/// A chunk that tokenizes well past the short group's padded length.
fn long_chunk(index: usize) -> String {
    let mut text = String::new();
    for line in 0..8 {
        text.push_str(&format!(
            "pub fn handler_{index}_{line}(request: &Request, state: &mut State) -> Result<Response, Error> {{ state.count += {line}; Ok(Response::ok()) }}\n"
        ));
    }
    text
}

/// The only merge FastEmbed's own contract preserves: `texts.chunks(k)` splits
/// a concatenation of equal-sized groups back into exactly the original
/// per-group forward passes, so every group keeps its own padded shape.
///
/// It is also why merging buys nothing. The concatenation still costs one
/// `ort::Session::run` per group; only the Rust-side call count drops.
fn assert_equal_size_merge_is_byte_identical(embedding: &mut TextEmbedding, label: &str) {
    let first: Vec<String> = (0..8).map(short_chunk).collect();
    let second: Vec<String> = (0..8).map(long_chunk).collect();
    let first_alone = embedding
        .embed(first.clone(), Some(8))
        .expect("first group alone");
    let second_alone = embedding
        .embed(second.clone(), Some(8))
        .expect("second group alone");
    let mut concatenated = first;
    concatenated.extend(second);
    let merged = embedding
        .embed(concatenated, Some(16))
        .expect("merged into one call under Some(16)");

    let mut separate = first_alone;
    separate.extend(second_alone);
    assert_eq!(
        separate.len(),
        merged.len(),
        "the chunked split must return one row per input text"
    );
    let (differing, worst) = compare(&separate, &merged);
    println!(
        "{label} EQUAL-SIZE-CHUNKED-MERGE: rows={} differing_lanes={differing} \
         max_abs_delta={worst:e}",
        separate.len()
    );
    assert_eq!(
        differing, 0,
        "FastEmbed's `texts.chunks(k)` split must reproduce each group's own \
         forward pass byte for byte; it is the only merge that preserves the \
         admitted tensor shape"
    );
}

/// Regroup short and long chunks into one forward pass and require the
/// cataloged Jina graph to preserve every emitted float bit.
fn assert_cross_group_merge_is_byte_identical(embedding: &mut TextEmbedding, label: &str) {
    // A five-chunk remainder from a multi-chunk file, then a 27-chunk group:
    // together exactly one admitted 32-wide batch.
    let remainder: Vec<String> = (0..5).map(short_chunk).collect();
    let full: Vec<String> = (0..PRODUCTION_BATCH_SIZE - 5).map(long_chunk).collect();
    let remainder_alone = embedding
        .embed(remainder.clone(), Some(remainder.len()))
        .expect("remainder group alone");
    let full_alone = embedding
        .embed(full.clone(), Some(full.len()))
        .expect("full group alone");

    let mut concatenated = remainder;
    concatenated.extend(full);
    let width = concatenated.len();
    let one_pass = embedding
        .embed(concatenated, Some(width))
        .expect("cross-group single forward pass");

    let mut separate = remainder_alone;
    separate.extend(full_alone);
    assert_eq!(
        separate.len(),
        width,
        "the per-group dispatch must return one row per input text"
    );
    assert_eq!(
        one_pass.len(),
        width,
        "the merged dispatch must return one row per input text"
    );
    assert_eq!(
        separate.first().map(Vec::len),
        one_pass.first().map(Vec::len),
        "a merge that changed the emitted dimensionality would make the \
         reported per-lane delta meaningless"
    );
    let (differing, worst) = compare(&separate, &one_pass);
    println!(
        "{label} CROSS-GROUP-ONE-PASS: rows={width} differing_lanes={differing} \
         max_abs_delta={worst:e} identical={}",
        differing == 0
    );
    assert_eq!(
        differing, 0,
        "token-length regrouping changed catalog-model vector bytes; bump the \
         canonical projection revision before accepting this change"
    );
}

#[test]
#[ignore = "requires a verified local FastEmbed fixture directory in \
            TRACEDECAY_FASTEMBED_FIXTURE_DIR; run with --ignored"]
fn length_bucket_regrouping_preserves_catalog_model_vector_bytes() {
    let root = env::var(FIXTURE_ENV).unwrap_or_else(|_| {
        panic!("this gate must provide its verified FastEmbed fixture in {FIXTURE_ENV}")
    });
    let root = Path::new(&root);
    for intra_threads in [1, PRODUCTION_INTRA_THREADS] {
        let label = format!("[intra_threads={intra_threads}]");
        let mut embedding = model(root, intra_threads);
        assert_equal_size_merge_is_byte_identical(&mut embedding, &label);
        assert_cross_group_merge_is_byte_identical(&mut embedding, &label);
    }
}
