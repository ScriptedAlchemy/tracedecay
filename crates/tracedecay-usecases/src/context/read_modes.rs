//! Root-free value operations for mode-aware file reads.

/// Mode selector for `tracedecay_read`. Parsed from the JSON `mode` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    Full,
    Lines,
    Map,
    Signatures,
}

impl ReadMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lines => "lines",
            Self::Map => "map",
            Self::Signatures => "signatures",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "full" => Some(Self::Full),
            "lines" => Some(Self::Lines),
            "map" => Some(Self::Map),
            "signatures" => Some(Self::Signatures),
            _ => None,
        }
    }
}

/// Inclusive 1-based line range parsed from `"A-B"` (or just `"A"` for a
/// single line). Out-of-range values are clamped at render time.
#[derive(Debug, Clone, Copy)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some((a, b)) = s.split_once('-') {
            let start: u32 = a.trim().parse().ok()?;
            let end: u32 = b.trim().parse().ok()?;
            if start == 0 || end < start {
                return None;
            }
            Some(Self { start, end })
        } else {
            let line: u32 = s.parse().ok()?;
            if line == 0 {
                return None;
            }
            Some(Self {
                start: line,
                end: line,
            })
        }
    }
}

/// Renders the `full` mode body — entire file content as UTF-8 text.
pub fn render_full(source: &str) -> String {
    source.to_string()
}

/// Approximates the token count of a UTF-8 string using chars/4.
pub fn estimate_tokens(s: &str) -> u32 {
    let chars = s.chars().count();
    chars.div_ceil(4).min(u32::MAX as usize) as u32
}

/// Renders the `lines` mode body for a 1-based inclusive range.
pub fn render_lines(source: &str, range: LineRange) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = (range.start.saturating_sub(1)) as usize;
    let end = (range.end as usize).min(lines.len());
    if start >= lines.len() || start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_modes() {
        assert_eq!(ReadMode::parse("full"), Some(ReadMode::Full));
        assert_eq!(ReadMode::parse("lines"), Some(ReadMode::Lines));
        assert_eq!(ReadMode::parse("map"), Some(ReadMode::Map));
        assert_eq!(ReadMode::parse("signatures"), Some(ReadMode::Signatures));
        assert_eq!(ReadMode::parse("nope"), None);
    }

    #[test]
    fn parses_valid_line_ranges() {
        let pair = LineRange::parse("3-5").unwrap();
        assert_eq!((pair.start, pair.end), (3, 5));
        let single = LineRange::parse("7").unwrap();
        assert_eq!((single.start, single.end), (7, 7));
    }

    #[test]
    fn rejects_invalid_line_ranges() {
        assert!(LineRange::parse("0").is_none());
        assert!(LineRange::parse("5-3").is_none());
        assert!(LineRange::parse("a-b").is_none());
    }

    #[test]
    fn renders_line_ranges_with_clamping() {
        let source = "alpha\nbeta\ngamma\n";
        assert_eq!(
            render_lines(source, LineRange { start: 2, end: 99 }),
            "beta\ngamma"
        );
        assert_eq!(render_lines(source, LineRange { start: 5, end: 8 }), "");
    }

    #[test]
    fn renders_full_source_and_estimates_tokens() {
        assert_eq!(render_full("hello\nworld\n"), "hello\nworld\n");
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }
}
