#[test]
fn query_leaf_has_no_policy_dependency() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read tracedecay-query manifest");

    assert!(
        !manifest.lines().any(|line| {
            let line = line.trim_start();
            !line.starts_with('#')
                && (line.starts_with("tracedecay-policy") || line.starts_with("tracedecay_policy"))
        }),
        "tracedecay-query must accept policy decisions through its public API"
    );
}
