use tracedecay::text::utf8_prefix_at_or_before as compatibility_utf8_prefix;
use tracedecay_runtime_core::text::utf8_prefix_at_or_before as core_utf8_prefix;

#[test]
fn text_compatibility_facade_reexports_runtime_core() {
    let text = "rust🦀";
    assert_eq!(
        compatibility_utf8_prefix(text, 5),
        core_utf8_prefix(text, 5)
    );
}
