//! Protocol-facing diagnostic merge and UTF-16 projection helpers.
//!
//! This is a bounded projection boundary, not a finding store. It reads
//! canonical diagnostics and feedback-cycle output and must not own diagnostic
//! lifecycle transitions or write a gateway-private database.

use std::collections::BTreeSet;

pub const MAX_DOCUMENT_DIAGNOSTICS: usize = 200;
pub const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 512;
pub const MAX_DIAGNOSTIC_RELATED_INFORMATION: usize = 8;
pub const MAX_DIAGNOSTIC_RELATED_MESSAGE_BYTES: usize = 256;
pub const MAX_DIAGNOSTIC_URI_BYTES: usize = 2_048;
pub const TRACEDECAY_DIAGNOSTIC_DATA_REVISION: u32 = 1;

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
///
/// The `TraceDecay` lane names its real producer: a review finding and a
/// CI-localization finding are distinct producers and must not both render as
/// an anonymous `tracedecay`. Every non-[`DiagnosticSource::Upstream`] variant
/// belongs to the `TraceDecay` lane and keeps its lane privileges.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticSource {
    Upstream,
    /// TraceDecay-relayed compiler/toolchain findings.
    TraceDecay,
    /// GitHub review advisory findings.
    TraceDecayGitHub,
    /// CI failure-localization advisory findings.
    TraceDecayCi,
    /// Proximity advisory findings.
    TraceDecayProximity,
}

impl DiagnosticSource {
    /// Whether this source belongs to the `TraceDecay` lane (as opposed to a
    /// host's own upstream language server).
    #[must_use]
    pub const fn is_tracedecay(self) -> bool {
        !matches!(self, Self::Upstream)
    }

    /// The exact wire string published in LSP `Diagnostic.source`.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Upstream => "upstream",
            Self::TraceDecay => "tracedecay",
            Self::TraceDecayGitHub => "tracedecay-github",
            Self::TraceDecayCi => "tracedecay-ci",
            Self::TraceDecayProximity => "tracedecay-proximity",
        }
    }

    /// Resolves the producer lane from a durable record's canonical
    /// `provenance.producer` identity. Unrecognized producers fall back to the
    /// generic `TraceDecay` lane rather than being dropped.
    #[must_use]
    pub fn from_producer(producer: &str) -> Self {
        match producer {
            "tracedecay-github" => Self::TraceDecayGitHub,
            "tracedecay-ci" => Self::TraceDecayCi,
            "tracedecay-proximity" => Self::TraceDecayProximity,
            _ => Self::TraceDecay,
        }
    }
}

/// Immutable identity needed to clear or reauthorize expansion of one
/// `TraceDecay` diagnostic. It intentionally contains no source text, evidence,
/// credentials, or mutable path identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GatewayDiagnosticIdentity {
    pub finding_id: String,
    pub anchor_id: String,
    pub generation: u64,
    pub head_commit_id: String,
    pub code_generation_id: String,
    pub snapshot_digest: String,
    pub invalidation_digest: String,
    pub snapshot_content_digest: String,
    pub document_content_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum GatewayDiagnosticLifecycle {
    Active,
    Superseded,
    Resolved,
    Cleared,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum GatewayDiagnosticProviderState {
    SupportedCompletedComplete,
    Unsupported,
    Absent,
    Indexing,
    Stale,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum GatewayDiagnosticCoverage {
    Complete,
    Partial,
    Unavailable,
    Failed,
}

/// Bounded allowlist for standard LSP `Diagnostic.data`.
///
/// The opaque handle is transport-only and is reauthorized by the existing
/// context expansion path; possession does not grant evidence access.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GatewayDiagnosticData {
    pub identity: GatewayDiagnosticIdentity,
    pub lifecycle: GatewayDiagnosticLifecycle,
    pub provider_state: GatewayDiagnosticProviderState,
    pub coverage: GatewayDiagnosticCoverage,
    pub expansion_handle: String,
}

/// One already-authorized, source-free location related to a diagnostic.
///
/// The application projection owns authorization. This protocol type only
/// enforces the bounded, credential-free wire contract.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GatewayDiagnosticRelatedInformation {
    pub uri: String,
    pub range: LspRange,
    pub message: String,
}

/// A protocol-facing diagnostic projection.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GatewayDiagnostic {
    pub uri: String,
    pub range: LspRange,
    pub severity: Option<DiagnosticSeverity>,
    pub code: Option<String>,
    pub code_description_uri: Option<String>,
    pub message: String,
    pub source: DiagnosticSource,
    pub related_information: Vec<GatewayDiagnosticRelatedInformation>,
    pub data: Option<GatewayDiagnosticData>,
}

impl GatewayDiagnostic {
    /// Normalizes one diagnostic into its merge lane.
    ///
    /// The lane is authoritative, but within the `TraceDecay` lane a producer
    /// already named by the projection is preserved — merging must not erase
    /// `tracedecay-github`/`tracedecay-ci`/`tracedecay-proximity` back into an
    /// anonymous `tracedecay`.
    fn normalize(mut self, lane: DiagnosticSource) -> Self {
        if lane.is_tracedecay() {
            if !self.source.is_tracedecay() {
                self.source = lane;
            }
            if self.severity.is_none() {
                self.severity = Some(DiagnosticSeverity::Information);
            }
        } else {
            self.source = lane;
            self.data = None;
        }
        self.code_description_uri = self
            .code_description_uri
            .filter(|uri| safe_code_description_uri(uri));
        self.related_information.retain_mut(|related| {
            if !safe_related_uri(&related.uri) || related.range.start > related.range.end {
                return false;
            }
            truncate_utf8(&mut related.message, MAX_DIAGNOSTIC_RELATED_MESSAGE_BYTES);
            true
        });
        self.related_information
            .truncate(MAX_DIAGNOSTIC_RELATED_INFORMATION);
        truncate_utf8(&mut self.message, MAX_DIAGNOSTIC_MESSAGE_BYTES);
        self
    }
}

pub(crate) fn safe_code_description_uri(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_DIAGNOSTIC_URI_BYTES {
        return false;
    }
    let Ok(uri) = url::Url::parse(value) else {
        return false;
    };
    uri.scheme() == "https"
        && uri.host_str().is_some()
        && uri.username().is_empty()
        && uri.password().is_none()
        && uri.port().is_none()
        && uri.query().is_none()
}

pub(crate) fn safe_related_uri(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_DIAGNOSTIC_URI_BYTES {
        return false;
    }
    let Ok(uri) = url::Url::parse(value) else {
        return false;
    };
    uri.scheme() == "file"
        && uri.username().is_empty()
        && uri.password().is_none()
        && uri.port().is_none()
        && uri.query().is_none()
        && uri.fragment().is_none()
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
    InsideLineEnding,
}

pub fn utf16_position_to_byte_offset(
    text: &str,
    position: LspPosition,
) -> Result<usize, PositionError> {
    let (line_start, line_end) = line_bounds(text, position.line)?;
    utf16_column_to_byte_offset(&text[line_start..line_end], line_start, position.character)
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
    let mut line_start = 0_usize;
    loop {
        let (line_end, next_line_start) = next_line_bounds(text, line_start);
        if offset <= line_end {
            let character = text[line_start..offset]
                .chars()
                .map(|value| value.len_utf16() as u32)
                .sum();
            return Ok(LspPosition { line, character });
        }
        let Some(next_line_start) = next_line_start else {
            return Err(PositionError::ByteOutOfBounds);
        };
        if offset < next_line_start {
            return Err(PositionError::InsideLineEnding);
        }
        line = line.saturating_add(1);
        line_start = next_line_start;
    }
}

fn line_bounds(text: &str, target_line: u32) -> Result<(usize, usize), PositionError> {
    let mut line = 0_u32;
    let mut line_start = 0_usize;
    loop {
        let (line_end, next_line_start) = next_line_bounds(text, line_start);
        if line == target_line {
            return Ok((line_start, line_end));
        }
        let Some(next_line_start) = next_line_start else {
            return Err(PositionError::LineOutOfBounds);
        };
        line = line.saturating_add(1);
        line_start = next_line_start;
    }
}

/// Returns the content end and the byte after the line terminator. LSP treats
/// CRLF as one line ending and also permits lone CR and LF endings.
fn next_line_bounds(text: &str, line_start: usize) -> (usize, Option<usize>) {
    let bytes = text.as_bytes();
    let mut index = line_start;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => return (index, Some(index + 1)),
            b'\r' => {
                let next = if bytes.get(index + 1) == Some(&b'\n') {
                    index + 2
                } else {
                    index + 1
                };
                return (index, Some(next));
            }
            _ => index += 1,
        }
    }
    (bytes.len(), None)
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

pub(crate) fn truncate_utf8(value: &mut String, max_bytes: usize) {
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
            code_description_uri: None,
            message: message.into(),
            source,
            related_information: Vec::new(),
            data: None,
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
    fn document_merge_drops_cross_document_and_invalid_ranges() {
        let mut cross_document = diagnostic(DiagnosticSource::Upstream, "other");
        cross_document.uri = "file:///root/b.rs".into();
        let mut invalid_range = diagnostic(DiagnosticSource::TraceDecay, "invalid");
        invalid_range.range.start.character = 2;
        invalid_range.range.end.character = 1;
        let merged = DiagnosticMerge::for_document(
            "file:///root/a.rs",
            vec![cross_document],
            vec![invalid_range],
        );

        assert!(merged.items.is_empty());
        assert_eq!(merged.omitted_count, 2);
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

    #[test]
    fn utf16_positions_treat_crlf_and_lone_cr_as_line_endings() {
        let text = "a🦀\r\nλ\rz";
        let first_end = utf16_position_to_byte_offset(
            text,
            LspPosition {
                line: 0,
                character: 3,
            },
        )
        .unwrap();
        assert_eq!(&text[first_end..], "\r\nλ\rz");
        assert_eq!(
            byte_offset_to_utf16_position(text, first_end),
            Ok(LspPosition {
                line: 0,
                character: 3,
            })
        );
        assert_eq!(
            byte_offset_to_utf16_position(text, first_end + 1),
            Err(PositionError::InsideLineEnding)
        );
        let third_line = text.find('z').unwrap();
        assert_eq!(
            byte_offset_to_utf16_position(text, third_line),
            Ok(LspPosition {
                line: 2,
                character: 0,
            })
        );
        assert_eq!(
            utf16_position_to_byte_offset(
                text,
                LspPosition {
                    line: 1,
                    character: 1,
                }
            ),
            Ok(text.find('\r').unwrap() + "\r\n".len() + 'λ'.len_utf8())
        );
    }
}

#[cfg(test)]
mod producer_source_tests {
    use super::*;

    fn diagnostic(source: DiagnosticSource) -> GatewayDiagnostic {
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
            code: Some(source.wire_name().to_owned()),
            code_description_uri: None,
            message: source.wire_name().to_owned(),
            source,
            related_information: Vec::new(),
            data: None,
        }
    }

    #[test]
    fn merging_preserves_each_producers_source() {
        let producers = [
            DiagnosticSource::TraceDecay,
            DiagnosticSource::TraceDecayGitHub,
            DiagnosticSource::TraceDecayCi,
            DiagnosticSource::TraceDecayProximity,
        ];
        let merged = DiagnosticMerge::new(
            vec![diagnostic(DiagnosticSource::Upstream)],
            producers.map(diagnostic).to_vec(),
        );
        for producer in producers {
            assert!(
                merged
                    .items
                    .iter()
                    .any(|item| item.source == producer && item.data.is_none()),
                "{} was collapsed during merge",
                producer.wire_name()
            );
        }
    }

    #[test]
    fn upstream_lane_always_wins_over_a_claimed_tracedecay_source() {
        let merged = DiagnosticMerge::new(vec![diagnostic(DiagnosticSource::TraceDecayCi)], vec![]);
        assert_eq!(merged.items[0].source, DiagnosticSource::Upstream);
    }

    #[test]
    fn producer_mapping_round_trips_and_defaults_safely() {
        for source in [
            DiagnosticSource::TraceDecay,
            DiagnosticSource::TraceDecayGitHub,
            DiagnosticSource::TraceDecayCi,
            DiagnosticSource::TraceDecayProximity,
        ] {
            assert_eq!(DiagnosticSource::from_producer(source.wire_name()), source);
            assert!(source.is_tracedecay());
        }
        assert_eq!(
            DiagnosticSource::from_producer("some-unknown-producer"),
            DiagnosticSource::TraceDecay
        );
        assert!(!DiagnosticSource::Upstream.is_tracedecay());
    }
}
