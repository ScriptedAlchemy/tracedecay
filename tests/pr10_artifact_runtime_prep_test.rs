#![forbid(unsafe_code)]

#[path = "../src/semantic_code/artifact_store.rs"]
mod artifact_store;
#[path = "../src/semantic_code/fastembed_adapter.rs"]
mod fastembed_adapter;
#[path = "../src/semantic_code/manifest.rs"]
mod manifest;
#[path = "../src/semantic_code/session_pool.rs"]
mod session_pool;
#[path = "../src/semantic_code/trust_roots.rs"]
mod trust_roots;

#[test]
fn quarantine_packet_remains_unwired_and_offline() {
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
        "FASTEMBED_CACHE_PATH",
    ] {
        assert!(
            !artifacts.contains(forbidden)
                && !runtime.contains(forbidden)
                && !pool.contains(forbidden),
            "quarantined artifact/runtime prep must not use network or ambient cache surface `{forbidden}`"
        );
    }
    assert!(
        !artifacts.contains("pub fn activate("),
        "Plan 20 owns profile activation; quarantined artifact prep must not publish it"
    );
}
