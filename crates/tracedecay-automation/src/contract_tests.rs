use crate::text::truncate_chars_for_prompt;

#[test]
fn prompt_truncation_counts_unicode_scalars() {
    assert_eq!(truncate_chars_for_prompt("a☺bc", 2), "a☺");
    assert_eq!(truncate_chars_for_prompt("a☺bc", 4), "a☺bc");
}
