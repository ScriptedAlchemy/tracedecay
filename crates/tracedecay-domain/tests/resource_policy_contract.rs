use tracedecay_domain::host_cpu_target;

#[test]
fn cpu_target_preserves_host_fallback_and_caller_cap() {
    let host_width = std::thread::available_parallelism().map_or(1, usize::from);

    assert_eq!(host_cpu_target(2), host_width.min(2));
    assert_eq!(host_cpu_target(usize::MAX), host_width);
}

#[test]
fn cpu_target_enforces_a_nonzero_cap() {
    assert_eq!(host_cpu_target(0), 1);
}
