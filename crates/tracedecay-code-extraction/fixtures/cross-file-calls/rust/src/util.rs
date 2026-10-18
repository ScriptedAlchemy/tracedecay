pub fn normalize(text: &str) -> String {
    text.trim().to_lowercase()
}

pub fn clamp(value: i64, low: i64, high: i64) -> i64 {
    value.max(low).min(high)
}
