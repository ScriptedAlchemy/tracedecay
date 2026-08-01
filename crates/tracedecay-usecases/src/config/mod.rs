//! Configuration surfaces the use-case layer owns or re-exports.
//!
//! `retrieval` and `scope_control` were moved down out of the root binary's
//! `src/config/`: their whole dependency closure is `tracedecay-domain`,
//! `tracedecay-query`, `tracedecay-search-eval`, `tracedecay-semantic` and this
//! crate's own `configuration` module, so nothing kept them at the root. The
//! remaining rows are re-exports of surfaces `tracedecay-global-db` and
//! `tracedecay-domain` already own, kept under the historical `crate::config::…`
//! spelling so the moved call sites did not have to churn.

pub mod retrieval;
pub mod scope_control;

pub use tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY;
pub use tracedecay_global_db::configuration::semantic::{
    SemanticConfig, SemanticProfileSelection, SemanticResourceCeilings,
};
pub use tracedecay_global_db::configuration::{registry, resolver};
