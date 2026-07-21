//! Protocol-facing diagnostic merge and UTF-16 projection helpers.
//!
//! This is a bounded projection boundary, not a finding store. It reads
//! canonical diagnostics and feedback-cycle output and must not own diagnostic
//! lifecycle transitions or write a gateway-private database.

use std::collections::BTreeSet;

pub const MAX_DOCUMENT_DIAGNOSTICS: usize = 200;
pub const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 512;

/// A zero-based LSP position using the negotiated UTF-16 encoding.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

/// An LSP range whose endpoints use [`LspPosition`].
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

/// LSP-compatible diagnostic severities.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// The source lane preserved while composing a document diagnostic report.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticSource {
    Upstream,
    TraceDecay,
}

/// A protocol-facing diagnostic projection.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GatewayDiagnostic {
    pub uri: String,
    pub range: LspRange,
    pub severity: Option<DiagnosticSeverity>,
    pub code: Option<String>,
    pub message: String,
    pub source: DiagnosticSource,
}

impl GatewayDiagnostic {
    fn normalize(mut self, source: DiagnosticSource) -> Self {
        self.source = source;
        if source == DiagnosticSource::TraceDecay && self.severity.is_none() {
            self.severity = Some(DiagnosticSeverity::Information);
        }
        truncate_utf8(&mut self.message, MAX_DIAGNOSTIC_MESSAGE_BYTES);
        self
    }
}

/// The two document diagnostic-report shapes used by LSP 3.17 pull
/// diagnostics. Full reports are generation-bound by a non-empty result id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentDiagnosticReport {
    Full {
        result_id: String,
        items: Vec<GatewayDiagnostic>,
    },
    Unchanged {
        result_id: String,
    },
}

impl DocumentDiagnosticReport {
    pub fn full(result_id: impl Into<String>, items: Vec<GatewayDiagnostic>) -> Self {
        let result_id = result_id.into();
        debug_assert!(
            !result_id.is_empty(),
            "diagnostic result ids are generation-bound"
        );
        Self::Full { result_id, items }
    }
}

/// Bounded, deterministic merge result. Omission counts are retained for the
/// daemon's typed status/Doctor projection rather than hidden in LSP data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticMerge {
    pub items: Vec<GatewayDiagnostic>,
    pub omitted_count: usize,
}

impl DiagnosticMerge {
    pub fn new(upstream: Vec<GatewayDiagnostic>, tracedecay: Vec<GatewayDiagnostic>) -> Self {
        Self::from_filtered(upstream, tracedecay, 0)
    }

    pub fn for_document(
        document_uri: &str,
        mut upstream: Vec<GatewayDiagnostic>,
        mut tracedecay: Vec<GatewayDiagnostic>,
    ) -> Self {
        let original_count = upstream.len() + tracedecay.len();
        let valid_for_document = |diagnostic: &GatewayDiagnostic| {
            diagnostic.uri == document_uri && diagnostic.range.start <= diagnostic.range.end
        };
        upstream.retain(valid_for_document);
        tracedecay.retain(valid_for_document);
        let filtered_count = upstream.len() + tracedecay.len();
        Self::from_filtered(
            upstream,
            tracedecay,
            original_count.saturating_sub(filtered_count),
        )
    }

    fn from_filtered(
        upstream: Vec<GatewayDiagnostic>,
        tracedecay: Vec<GatewayDiagnostic>,
        filtered_count: usize,
    ) -> Self {
        let mut unique = BTreeSet::new();
        unique.extend(
            upstream
                .into_iter()
                .map(|diagnostic| diagnostic.normalize(DiagnosticSource::Upstream)),
        );
        unique.extend(
            tracedecay
                .into_iter()
                .map(|diagnostic| diagnostic.normalize(DiagnosticSource::TraceDecay)),
        );

        let omitted_count = filtered_count + unique.len().saturating_sub(MAX_DOCUMENT_DIAGNOSTICS);
        let items = unique.into_iter().take(MAX_DOCUMENT_DIAGNOSTICS).collect();
        Self {
            items,
            omitted_count,
        }
    }

    pub fn into_items(self) -> Vec<GatewayDiagnostic> {
        self.items
    }
}

pub fn merge_diagnostics(
    upstream: Vec<GatewayDiagnostic>,
    tracedecay: Vec<GatewayDiagnostic>,
) -> DiagnosticMerge {
    DiagnosticMerge::new(upstream, tracedecay)
}

/// UTF position conversion failures are explicit; positions inside a surrogate
/// pair are never rounded to a neighboring byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionError {
    LineOutOfBounds,
    CharacterOutOfBounds,
    InsideSurrogatePair,
    ByteOutOfBounds,
    NotUtf8Boundary,
}

pub fn utf16_position_to_byte_offset(
    text: &str,
    position: LspPosition,
) -> Result<usize, PositionError> {
    let mut line = 0_u32;
    let mut line_start = 0_usize;
    for (offset, character) in text.char_indices() {
        if line == position.line {
            let line_end = text[offset..]
                .find('\n')
                .map_or(text.len(), |relative| offset + relative);
            return utf16_column_to_byte_offset(
                &text[line_start..line_end],
                line_start,
                position.character,
            );
        }
        if character == '\n' {
            line += 1;
            line_start = offset + character.len_utf8();
        }
    }

    if line == position.line {
        return utf16_column_to_byte_offset(&text[line_start..], line_start, position.character);
    }
    Err(PositionError::LineOutOfBounds)
}

pub fn byte_offset_to_utf16_position(
    text: &str,
    offset: usize,
) -> Result<LspPosition, PositionError> {
    if offset > text.len() {
        return Err(PositionError::ByteOutOfBounds);
    }
    if !text.is_char_boundary(offset) {
        return Err(PositionError::NotUtf8Boundary);
    }

    let mut line = 0_u32;
    let mut character = 0_u32;
    for value in text[..offset].chars() {
        if value == '\n' {
            line += 1;
            character = 0;
        } else {
            character += value.len_utf16() as u32;
        }
    }
    Ok(LspPosition { line, character })
}

fn utf16_column_to_byte_offset(
    line: &str,
    line_start: usize,
    target: u32,
) -> Result<usize, PositionError> {
    let mut units = 0_u32;
    for (offset, value) in line.char_indices() {
        if units == target {
            return Ok(line_start + offset);
        }
        let next = units + value.len_utf16() as u32;
        if target < next {
            return Err(PositionError::InsideSurrogatePair);
        }
        units = next;
    }
    if units == target {
        Ok(line_start + line.len())
    } else {
        Err(PositionError::CharacterOutOfBounds)
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(source: DiagnosticSource, message: impl Into<String>) -> GatewayDiagnostic {
        GatewayDiagnostic {
            uri: "file:///root/a.rs".into(),
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 1,
                },
            },
            severity: None,
            code: Some("test".into()),
            message: message.into(),
            source,
        }
    }

    #[test]
    fn merge_is_deterministic_deduplicated_and_provenance_preserving() {
        let upstream = diagnostic(DiagnosticSource::TraceDecay, "same");
        let tracedecay = diagnostic(DiagnosticSource::Upstream, "same");
        let merged = merge_diagnostics(
            vec![upstream.clone(), upstream],
            vec![tracedecay.clone(), tracedecay],
        );

        assert_eq!(merged.items.len(), 2);
        assert_eq!(merged.items[0].source, DiagnosticSource::Upstream);
        assert_eq!(merged.items[0].severity, None);
        assert_eq!(merged.items[1].source, DiagnosticSource::TraceDecay);
        assert_eq!(
            merged.items[1].severity,
            Some(DiagnosticSeverity::Information)
        );
    }

    #[test]
    fn merge_reports_bounded_omissions_and_truncates_on_utf8_boundary() {
        let diagnostics = (0..=MAX_DOCUMENT_DIAGNOSTICS)
            .map(|index| {
                diagnostic(
                    DiagnosticSource::TraceDecay,
                    format!("{index:03}{}", "🦀".repeat(200)),
                )
            })
            .collect();
        let merged = merge_diagnostics(Vec::new(), diagnostics);

        assert_eq!(merged.items.len(), MAX_DOCUMENT_DIAGNOSTICS);
        assert_eq!(merged.omitted_count, 1);
        assert!(
            merged
                .items
                .iter()
                .all(|item| item.message.len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES)
        );
    }

    #[test]
    fn utf16_positions_round_trip_across_astral_unicode_and_lines() {
        let text = "a🦀b\nλz";
        let position = LspPosition {
            line: 0,
            character: 3,
        };
        let offset = utf16_position_to_byte_offset(text, position).unwrap();
        assert_eq!(&text[offset..], "b\nλz");
        assert_eq!(byte_offset_to_utf16_position(text, offset), Ok(position));
        assert_eq!(
            utf16_position_to_byte_offset(
                text,
                LspPosition {
                    line: 0,
                    character: 2,
                }
            ),
            Err(PositionError::InsideSurrogatePair)
        );
        assert_eq!(
            byte_offset_to_utf16_position(text, text.find('z').unwrap()),
            Ok(LspPosition {
                line: 1,
                character: 1,
            })
        );
    }
}
