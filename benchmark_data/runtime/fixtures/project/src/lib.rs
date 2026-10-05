pub fn fixture_catalog_total(quantities: &[usize]) -> usize {
    let feedback_bench_unused_probe = 7_u32;
    quantities.iter().copied().sum()
}

pub struct FixtureOptions {
    pub retries: usize,
    pub enabled: bool,
}

pub fn fixture_options() -> FixtureOptions {
    FixtureOptions {
        retries: 8,
        enabled: true,
    }
}

pub fn fixture_cli_flag() -> &'static str {
    "--fixture-catalog-mode"
}

pub fn fixture_unsafe_probe() -> u8 {
    let value = 7_u8;
    let pointer = &value as *const u8;
    // SAFETY: pointer is derived from a live local for this read.
    unsafe { *pointer }
}
