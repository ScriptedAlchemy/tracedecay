#[test]
fn lcm_modules_do_not_depend_on_context_builder_or_memory_fact_store() {
    for path in [
        "src/sessions/lcm/raw.rs",
        "src/sessions/lcm/dag.rs",
        "src/sessions/lcm/query.rs",
        "src/sessions/lcm/compression.rs",
    ] {
        let source = std::fs::read_to_string(path).unwrap();
        assert!(
            !source.contains("ContextBuilder"),
            "{path} must stay independent from codegraph context assembly"
        );
        assert!(
            !source.contains("MemoryCategory"),
            "{path} must stay independent from memory fact categories"
        );
        assert!(
            !source.contains("memory_facts"),
            "{path} must not store LCM summaries in fact memory tables"
        );
    }
}
