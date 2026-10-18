pub fn normalize(text: &str) -> String {
    text.to_lowercase()
}

pub fn old_format(value: &str) -> String {
    format!("<{}>", normalize(value))
}
