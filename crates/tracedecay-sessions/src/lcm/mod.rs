//! Provider-neutral LCM contracts and reducers.

pub mod compression_policy;
pub mod contracts;
pub mod replay_transactions;
pub mod security;

/// The LCM token-budget heuristic: whitespace-delimited words, never zero.
///
/// Every LCM budget decision is denominated in this unit — the compression
/// trigger, the replay accounting, the retrieval window, and the policy
/// reducer. It lives here, above both the contract reducers and the runtime,
/// because the four of them must agree: a heuristic that only some callers
/// adopt would let a session compress against one budget and be replayed
/// against another.
///
/// Named distinctly from the chars/4 `estimate_tokens` helpers in read-mode
/// and global-db surfaces so those cannot be imported into this budget path
/// by accident.
pub(crate) fn lcm_budget_tokens(text: &str) -> i64 {
    text.split_whitespace().count().max(1) as i64
}
