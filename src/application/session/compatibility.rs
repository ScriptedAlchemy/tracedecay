use sha2::{Digest, Sha256};

const MAX_DERIVED_TEXT_CHARS: usize = 64 * 1024;
const MAX_DERIVED_SNIPPET_CHARS: usize = 4 * 1024;
const DERIVED_TRUNCATION_MARKER: &str = "\n[derived snippet truncated by tracedecay]";

pub fn projected_content_hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

pub fn derived_text_for_index(raw: &str) -> String {
    derived_text_with_cap(raw, MAX_DERIVED_TEXT_CHARS)
}

pub fn derived_text_for_snippet(raw: &str) -> String {
    derived_text_with_cap(raw, MAX_DERIVED_SNIPPET_CHARS)
}

fn derived_text_with_cap(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }

    let marker_chars = DERIVED_TRUNCATION_MARKER.chars().count();
    let budget = max_chars.saturating_sub(marker_chars);
    let mut derived = raw.chars().take(budget).collect::<String>();
    derived.push_str(DERIVED_TRUNCATION_MARKER);
    derived
}
