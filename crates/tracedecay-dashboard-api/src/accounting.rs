pub mod metrics {
    /// Parse a dashboard range into a Unix timestamp.
    pub fn parse_range(range: &str) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match range {
            "today" => now - (now % 86_400),
            "30d" | "month" => now.saturating_sub(30 * 86_400),
            "all" => 0,
            _ => now.saturating_sub(7 * 86_400),
        }
    }
}
