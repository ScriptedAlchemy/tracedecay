pub fn normalize_entity(entity: &str) -> String {
    entity
        .trim_matches(|c: char| {
            c.is_ascii_punctuation() && c != '_' && c != '/' && c != '\\' && c != ':' && c != '.'
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
