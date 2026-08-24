use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use tracedecay::agents::host_bundle_registry::FIRST_PARTY_COMPONENT_CATALOG_VERSION;

fn read(root: &Path, name: &str) -> Vec<u8> {
    fs::read(root.join(name)).unwrap_or_else(|error| {
        panic!(
            "cannot read verified FastEmbed fixture member {}: {error}",
            root.join(name).display()
        )
    })
}

fn local_model(root: &Path, model: Vec<u8>) -> UserDefinedEmbeddingModel {
    UserDefinedEmbeddingModel::new(
        model,
        TokenizerFiles {
            tokenizer_file: read(root, "tokenizer.json"),
            config_file: read(root, "config.json"),
            special_tokens_map_file: read(root, "special_tokens_map.json"),
            tokenizer_config_file: read(root, "tokenizer_config.json"),
        },
    )
    .with_pooling(Pooling::Mean)
}

fn main() {
    assert!(
        FIRST_PARTY_COMPONENT_CATALOG_VERSION > 0,
        "the packaged tracedecay library must be linked"
    );
    let mut arguments = env::args_os().skip(1);
    let fixture = PathBuf::from(arguments.next().expect("fixture directory argument"));
    let expected_dimensions: usize = arguments
        .next()
        .expect("expected dimensions argument")
        .to_string_lossy()
        .parse()
        .expect("expected dimensions must be an integer");
    let configured_max_length: usize = arguments
        .next()
        .expect("maximum sequence length argument")
        .to_string_lossy()
        .parse()
        .expect("maximum sequence length must be an integer");
    assert!(
        arguments.next().is_none(),
        "unexpected acceptance arguments"
    );
    let model_info = TextEmbedding::get_model_info(&EmbeddingModel::JinaEmbeddingsV2BaseCode)
        .expect("FastEmbed must catalog the selected production code model");
    assert_eq!(model_info.dim, expected_dimensions);
    assert_eq!(model_info.model_code, "jinaai/jina-embeddings-v2-base-code");
    assert_eq!(model_info.model_file, "onnx/model.onnx");

    let bounded_max_length = configured_max_length.min(32);
    let options = InitOptionsUserDefined::new()
        .with_max_length(bounded_max_length)
        .with_intra_threads(1);
    let mut embedding = TextEmbedding::try_new_from_user_defined(
        local_model(&fixture, read(&fixture, "model.onnx")),
        options,
    )
    .expect("verified local FastEmbed fixture must initialize bundled ORT");

    let input = vec!["bounded offline distribution inference".to_owned()];
    assert!(input[0].len() <= 64);
    let vectors = embedding
        .embed(&input, Some(1))
        .expect("bundled ORT must execute bounded local inference");
    assert_eq!(vectors.len(), 1, "one input must produce one vector");
    assert_eq!(
        vectors[0].len(),
        expected_dimensions,
        "fixture metadata must match the emitted vector dimension"
    );
    assert!(
        vectors[0].iter().all(|value| value.is_finite()),
        "inference must emit only finite values"
    );

    let invalid = TextEmbedding::try_new_from_user_defined(
        local_model(&fixture, b"not an ONNX model".to_vec()),
        InitOptionsUserDefined::new()
            .with_max_length(bounded_max_length)
            .with_intra_threads(1),
    );
    assert!(
        invalid.is_err(),
        "invalid explicitly supplied model bytes must be unavailable"
    );
}
