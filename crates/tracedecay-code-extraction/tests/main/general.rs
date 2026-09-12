use tracedecay_code_extraction::{LanguageExtractor, LanguageRegistry, RustExtractor};
use tracedecay_domain::*;

#[test]
fn test_extract_derive_macros() {
    let source = r#"
#[derive(Debug, Clone, Serialize)]
pub struct Config { pub name: String }
"#;
    let result = RustExtractor.extract("src/config.rs", source);
    let derives: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::DerivesMacro)
        .collect();
    assert!(
        !derives.is_empty(),
        "should have derives_macro unresolved refs"
    );
    let names: Vec<&str> = derives.iter().map(|r| r.reference_name.as_str()).collect();
    assert!(names.contains(&"Debug"));
    assert!(names.contains(&"Clone"));
    assert!(names.contains(&"Serialize"));
}

#[test]
fn test_qualified_names() {
    let source = r#"
mod server {
    pub fn handle_request() {}
}
"#;
    let result = RustExtractor.extract("src/lib.rs", source);
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 1);
    assert!(fns[0].qualified_name.contains("server"));
    assert!(fns[0].qualified_name.contains("handle_request"));
}

#[test]
fn test_language_registry_finds_scala_extractor() {
    let registry = LanguageRegistry::new();
    assert!(registry.extractor_for_file("Main.scala").is_some());
    assert!(
        registry
            .extractor_for_file("src/com/example/App.scala")
            .is_some()
    );
    assert!(registry.extractor_for_file("script.sc").is_some());
}

#[test]
fn test_language_registry_returns_none_for_unknown() {
    let registry = LanguageRegistry::new();
    assert!(registry.extractor_for_file("style.css").is_none());
    assert!(registry.extractor_for_file("README.unknown").is_none());
}
