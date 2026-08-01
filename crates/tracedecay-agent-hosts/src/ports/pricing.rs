//! Model turn pricing.
//!
//! A **registered port**. `accounting::pricing` owns a `LiteLLM`-backed price
//! table with an on-disk cache and a network refresh path; Context Scout needs
//! exactly one read from it — the dollar cost of a turn — to stamp a receipt.
//!
//! Root wiring: the root registers [`register`] with
//! `accounting::pricing::cost_of_turn` during startup.
//!
//! Unregistered, the cost reads as `0.0`. That matches the root
//! implementation's own answer for a model missing from the price table, so an
//! unwired build records "cost unknown" rather than an invented figure.

use std::sync::OnceLock;

/// Computes the dollar cost of one model turn.
///
/// Arguments are `(model, input_tokens, output_tokens, cache_write_tokens,
/// cache_read_tokens)`.
pub type CostOfTurn = fn(&str, u64, u64, u64, u64) -> f64;

static COST_OF_TURN: OnceLock<CostOfTurn> = OnceLock::new();

/// Registers the root crate's turn-pricing reader.
///
/// Idempotent: the first registration wins.
pub fn register(cost_of_turn: CostOfTurn) {
    let _ = COST_OF_TURN.set(cost_of_turn);
}

/// Dollar cost of one turn, or `0.0` when the root never registered or the
/// model is absent from the price table.
#[must_use]
pub fn cost_of_turn(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    COST_OF_TURN.get().map_or(0.0, |cost| {
        cost(
            model,
            input_tokens,
            output_tokens,
            cache_write_tokens,
            cache_read_tokens,
        )
    })
}
