//! Compatibility facade for structural code search owned by `tracedecay-code-index`.

pub(crate) use tracedecay_code_index::search_tree_scoped_with_cancel;
pub use tracedecay_code_index::{
    AstGrepSearchError, AstGrepSearchMatch, AstGrepSearchResult, search_tree,
};
