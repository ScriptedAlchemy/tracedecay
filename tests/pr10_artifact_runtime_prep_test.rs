#![forbid(unsafe_code)]

#[path = "../src/semantic_code/artifact_store.rs"]
mod artifact_store;
#[path = "../src/semantic_code/fastembed_adapter.rs"]
mod fastembed_adapter;
#[path = "../src/semantic_code/manifest.rs"]
mod manifest;
#[path = "../src/semantic_code/model_catalog.rs"]
pub mod model_catalog;
#[path = "../src/semantic_code/runtime_query.rs"]
mod runtime_query;
#[path = "../src/semantic_code/runtime_service.rs"]
mod runtime_service;
#[path = "../src/semantic_code/session_pool.rs"]
mod session_pool;
#[path = "../src/semantic_code/trust_roots.rs"]
mod trust_roots;

pub mod query {
    pub use tracedecay::query::*;
}

// The included sources resolve `crate::config` and
// `crate::semantic_code::model_catalog` against this test crate's root, so
// mirror those paths onto the real lib module and the path-included copy.
pub mod config {
    pub use tracedecay::config::*;
}

pub mod semantic_code {
    pub use crate::model_catalog;
}

#[test]
fn semantic_runtime_uses_verified_fastembed_bytes_without_network_or_fallback() {
    let artifacts = include_str!("../src/semantic_code/artifact_store.rs");
    let runtime = include_str!("../src/semantic_code/fastembed_adapter.rs");
    let pool = include_str!("../src/semantic_code/session_pool.rs");
    let production = |source: &'static str| source.split("#[cfg(test)]").next().unwrap_or(source);
    let artifacts = production(artifacts);
    let runtime = production(runtime);
    let pool = production(pool);

    for forbidden in [
        "reqwest",
        "ureq",
        "hf_hub",
        "HUGGINGFACE_HUB_CACHE",
        "HF_HOME",
        "HF_ENDPOINT",
        "FASTEMBED_CACHE_DIR",
        "FASTEMBED_CACHE_PATH",
    ] {
        assert!(
            !artifacts.contains(forbidden)
                && !runtime.contains(forbidden)
                && !pool.contains(forbidden),
            "root-private artifact/runtime code must not use network or ambient cache surface `{forbidden}`"
        );
    }
    assert!(
        runtime.contains("try_new_from_user_defined("),
        "the semantic runtime must initialize FastEmbed from verified local artifact bytes"
    );
    assert!(
        !runtime.contains("TextEmbedding::try_new("),
        "the semantic runtime must not enable FastEmbed's model-download constructor"
    );
    assert!(
        !runtime.contains("FakeEmbeddingRuntime"),
        "a production semantic runtime must not fall back to pseudo embeddings"
    );
    assert!(
        !artifacts.contains("pub fn activate("),
        "Plan 20 owns profile activation; the artifact store must not publish it"
    );
}
