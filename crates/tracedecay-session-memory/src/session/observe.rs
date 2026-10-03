//! Metrics gauges owned by session retrieval.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Every macro expands to a no-op unless
//! the `metrics` recorder is installed.

use tracedecay_contracts::retrieval::SessionRetrievalBudgetStageV1;
