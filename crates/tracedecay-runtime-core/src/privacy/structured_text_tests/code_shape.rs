use tracedecay_domain::SanitizerDispositionV1;

use super::{
    StructuredTextFormatV1, sanitize_code_source_bytes, sanitize_provider_metadata_text,
    sanitize_structured_text,
};

fn assert_ordinary_rust_source_is_preserved(raw: &str) {
    let sanitized = sanitize_code_source_bytes(raw.as_bytes()).expect("ordinary Rust sanitizes");

    assert_eq!(sanitized.sanitized_bytes(), raw.as_bytes());
    assert_eq!(
        sanitized.receipt().disposition(),
        SanitizerDispositionV1::Accepted
    );
}

#[test]
fn code_source_preserves_struct_fields_after_inner_doc_comments() {
    assert_ordinary_rust_source_is_preserved(
        "//! A production record.\n\npub struct Record {\n    pub values: Vec<String>,\n}\n",
    );
}

#[test]
fn code_source_preserves_struct_literals_after_inner_doc_comments() {
    assert_ordinary_rust_source_is_preserved(
        "//! Builds a production record.\n\nRecord {\n    values: Vec::new(),\n}\n",
    );
}

#[test]
fn code_source_preserves_vec_types_after_inner_doc_comments() {
    assert_ordinary_rust_source_is_preserved(
        "//! Rows returned by the query.\n\npub type Rows = Vec<(String, usize)>;\n",
    );
}

#[test]
fn code_source_preserves_match_arms_after_inner_doc_comments() {
    assert_ordinary_rust_source_is_preserved(
        "//! Maps an optional value.\n\nmatch value {\n    Some(value) => value,\n    _ => return None,\n}\n",
    );
}

#[test]
fn code_shape_probe_retains_established_yaml_dotenv_and_provider_metadata() {
    let yaml = "---\nscript: |\n  fn main() { return; }\nregion: us-east\n";
    let dotenv = "# service configuration\nREGION=us-east\nRUST_LOG=debug\n";

    let yaml_scan = sanitize_structured_text(yaml).expect("YAML scan runs");
    assert_eq!(yaml_scan.format(), Some(StructuredTextFormatV1::Yaml));
    assert_eq!(yaml_scan.sanitized_text(), yaml);

    let dotenv_scan = sanitize_structured_text(dotenv).expect("dotenv scan runs");
    assert_eq!(dotenv_scan.format(), Some(StructuredTextFormatV1::Dotenv));
    assert_eq!(dotenv_scan.sanitized_text(), dotenv);

    assert!(
        sanitize_provider_metadata_text("provider:\n  vault_passphrase: ordinary-value\n")
            .is_some_and(|sanitized| !sanitized.contains("ordinary-value"))
    );
}
