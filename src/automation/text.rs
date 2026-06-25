pub(crate) fn truncate_chars_for_prompt(value: &str, max_chars: usize) -> String {
    if value.chars().nth(max_chars).is_none() {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}
