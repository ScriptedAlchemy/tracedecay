use std::path::Path;

use tracedecay_code_index::{AstGrepSearchError, search_tree};

#[test]
fn structural_search_public_surface_is_available() {
    let result = search_tree(Path::new("."), "fn $A() { $$$ }", Some("rust"), None, 1);
    assert!(!matches!(result, Err(AstGrepSearchError::EmptyPattern)));
}
