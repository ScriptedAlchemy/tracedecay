//! Model turn pricing.
//!
//! Context Scout reads the same all-provider authority as costs, CLI, MCP, and
//! HTTP. Unknown models stay unavailable instead of becoming zero-dollar work.

/// Dollar cost of one turn, or `None` when its provider/model is unpriced.
#[must_use]
pub fn cost_of_turn(
    provider: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
) -> Option<f64> {
    let table = tracedecay_usecases::provider_pricing::load_table();
    tracedecay_usecases::provider_pricing::cost_of_usage(
        table,
        provider,
        model,
        input_tokens,
        output_tokens,
        Some(cache_read_tokens),
        Some(cache_write_tokens),
    )
}
