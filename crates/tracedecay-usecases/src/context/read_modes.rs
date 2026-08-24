use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::errors::Result;

const MAX_CONTEXT_SYMBOLS: usize = 12;
const MAX_FILE_SYMBOLS: usize = 100_000;

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

#[derive(Debug, Clone, Copy)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn parse(s: &str) -> Option<Self> {
        if let Some((a, b)) = s.trim().split_once('-') {
            let start = a.trim().parse().ok()?;
            let end = b.trim().parse().ok()?;
            (start > 0 && end >= start).then_some(Self { start, end })
        } else {
            let line = s.trim().parse().ok()?;
            (line > 0).then_some(Self {
                start: line,
                end: line,
            })
        }
    }
}

pub fn render_full(source: &str) -> String {
    source.to_owned()
}
pub fn estimate_tokens(s: &str) -> u32 {
    s.chars().count().div_ceil(4).min(u32::MAX as usize) as u32
}
pub fn render_lines(source: &str, range: LineRange) -> String {
    let lines = source.lines().collect::<Vec<_>>();
    let start = range.start.saturating_sub(1) as usize;
    let end = (range.end as usize).min(lines.len());
    if start >= end {
        String::new()
    } else {
        lines[start..end].join("\n")
    }
}

pub fn render_map(
    reader: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    file_path: &str,
    kinds: Option<&[String]>,
) -> Result<Value> {
    let nodes = fetch_nodes(reader, cancellation, file_path)?;
    Ok(map_value(&nodes, file_path, kinds))
}

/// Publishes every signature the projection carries for one file.
///
/// The projection's per-symbol metadata (`LineageSymbolRecordV1`) already
/// carries `signature`, so this reads the same symbol page `render_map` reads
/// and keeps the symbols that declare one. Symbols whose extractor publishes no
/// signature are counted in `without_signature` rather than emitted with a null
/// signature, so an empty `symbols` list is never mistaken for an empty file.
pub fn render_signatures(
    reader: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    file_path: &str,
) -> Result<Value> {
    let nodes = fetch_nodes(reader, cancellation, file_path)?;
    Ok(signatures_value(&nodes, file_path))
}

/// Symbol context for a source read, optionally scoped to the read's lines.
///
/// Symbol context is an *optional enrichment* of a source read: it must never
/// fail the read that carries it. When a line range is requested and the
/// projection publishes usable line spans, the list is narrowed to the symbols
/// overlapping that range and the range is echoed back. When no selected symbol
/// carries a usable span the call degrades to the whole-file list with a null
/// `range` and an explanatory `note` instead of erroring.
pub fn render_symbol_context(
    reader: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    file_path: &str,
    range: Option<LineRange>,
) -> Result<Value> {
    let nodes = fetch_nodes(reader, cancellation, file_path)?;
    Ok(symbol_context_value(&nodes, file_path, range))
}

/// Builds the `mode="map"` payload from an already-fetched symbol page.
fn map_value(
    nodes: &[CodeGraphSymbolSummaryV1],
    file_path: &str,
    kinds: Option<&[String]>,
) -> Value {
    let symbols = nodes
        .iter()
        .filter(|node| {
            kinds.is_none_or(|kinds| {
                kinds.is_empty()
                    || kinds.iter().any(|kind| {
                        node.metadata
                            .as_ref()
                            .is_some_and(|metadata| metadata.kind.eq_ignore_ascii_case(kind))
                    })
            })
        })
        .map(symbol_entry)
        .collect::<Vec<_>>();
    json!({"file": file_path, "symbol_count": symbols.len(), "symbols": symbols})
}

/// Builds the `mode="signatures"` payload from an already-fetched symbol page.
fn signatures_value(nodes: &[CodeGraphSymbolSummaryV1], file_path: &str) -> Value {
    let mut symbols = Vec::new();
    let mut without_signature = 0usize;
    for node in nodes {
        match node
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.signature.as_deref())
            .filter(|signature| !signature.trim().is_empty())
        {
            Some(_) => symbols.push(symbol_entry(node)),
            None => without_signature += 1,
        }
    }
    json!({
        "file": file_path,
        "symbol_count": symbols.len(),
        "without_signature": without_signature,
        "symbols": symbols,
    })
}

/// Builds the symbol-context payload from an already-fetched symbol page,
/// scoping to `range` when the projection makes that possible.
fn symbol_context_value(
    nodes: &[CodeGraphSymbolSummaryV1],
    file_path: &str,
    range: Option<LineRange>,
) -> Value {
    let (selected, range_applied) = scope_to_range(nodes, range);
    let symbols = selected
        .iter()
        .take(MAX_CONTEXT_SYMBOLS)
        .map(|node| symbol_entry(node))
        .collect::<Vec<_>>();
    let range_value = match (range, range_applied) {
        (Some(range), true) => json!({"start": range.start, "end": range.end}),
        _ => Value::Null,
    };
    let mut value = json!({
        "file": file_path,
        "range": range_value,
        "symbol_count": selected.len(),
        "truncated": selected.len() > symbols.len(),
        "symbols": symbols,
    });
    if range.is_some() && !range_applied {
        value["note"] = json!(
            "line spans are not published for this file's symbols; showing whole-file symbol context"
        );
    }
    value
}

/// Selects the symbols a context payload should show, ordered nearest-first.
///
/// Returns the selection plus whether the requested range was actually applied.
/// A range narrows the selection only when at least one symbol publishes a
/// usable line span; otherwise the whole page is returned so the caller can
/// degrade instead of failing.
fn scope_to_range(
    nodes: &[CodeGraphSymbolSummaryV1],
    range: Option<LineRange>,
) -> (Vec<&CodeGraphSymbolSummaryV1>, bool) {
    let mut selected = nodes.iter().collect::<Vec<_>>();
    let range_applied = match range {
        Some(range) if nodes.iter().any(|node| symbol_line_span(node).is_some()) => {
            selected.retain(|node| {
                symbol_line_span(node)
                    .is_some_and(|(start, end)| start <= range.end && end >= range.start)
            });
            true
        }
        _ => false,
    };
    // Nearest-first so the `MAX_CONTEXT_SYMBOLS` cut keeps the symbols closest
    // to the top of the read; span-less symbols sort last in canonical order.
    selected.sort_by_key(|node| symbol_line_span(node).unwrap_or((u32::MAX, u32::MAX)));
    (selected, range_applied)
}

/// One symbol's 1-based inclusive line span, when the projection published a
/// usable one.
///
/// `LineageSymbolRecordV1::start_line` is the extractor's 0-based row and
/// `line_span` counts lines inclusively, so the display span is
/// `start_line + 1 ..= start_line + line_span` — the same `+ 1` convention the
/// verified graph handlers use. A zero span or an overflowing sum is a
/// degenerate record: this yields `None` rather than an error, because symbol
/// enrichment must never fail the source read that carries it.
fn symbol_line_span(node: &CodeGraphSymbolSummaryV1) -> Option<(u32, u32)> {
    let metadata = node.metadata.as_ref()?;
    if metadata.line_span == 0 {
        return None;
    }
    let start = metadata.start_line.checked_add(1)?;
    let end = start.checked_add(metadata.line_span - 1)?;
    Some((start, end))
}

fn fetch_nodes(
    reader: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    file_path: &str,
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    reader
        .symbols_in_logical_file(file_path, MAX_FILE_SYMBOLS, cancellation)
        .map_err(|error| {
            super::super::graph::map_code_graph_read_runtime_error(
                super::super::graph::map_projection_error(error),
            )
        })
}

/// One symbol row. `line`/`end_line` are 1-based inclusive and null when the
/// record carries no usable span; `signature` is null when the extractor
/// published none.
fn symbol_entry(node: &CodeGraphSymbolSummaryV1) -> Value {
    let span = node
        .binding
        .as_ref()
        .and_then(|binding| binding.source_span);
    let lines = symbol_line_span(node);
    let metadata = node.metadata.as_ref();
    json!({
        "id": node.occurrence.as_str(),
        "kind": metadata.map(|metadata| metadata.kind.as_str()),
        "name": metadata.map(|metadata| metadata.simple_name.as_str()),
        "qualified_name": metadata.map(|metadata| metadata.qualified_name.as_str()),
        "visibility": metadata.map(|metadata| metadata.visibility.as_str()),
        "line": lines.map(|(start, _)| start),
        "end_line": lines.map(|(_, end)| end),
        "signature": metadata.and_then(|metadata| metadata.signature.as_deref()),
        "start_byte": span.map(|span| span.start_byte),
        "end_byte": span.map(|span| span.end_byte),
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use tracedecay_code_index::graph_projection::CodeGraphSymbolBindingV1;
    use tracedecay_code_index::lineage::LineageSymbolRecordV1;
    use tracedecay_domain::SourceSpan;

    use super::*;

    const FILE: &str = "src/main.rs";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest<T>(byte: char) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid fixture digest")
    }

    /// One published symbol. `start_line` is the extractor's 0-based row and
    /// `line_span` is inclusive, matching what the projection stores.
    fn symbol(
        occurrence: &str,
        name: &str,
        kind: &str,
        start_line: u32,
        line_span: u32,
        signature: Option<&str>,
    ) -> CodeGraphSymbolSummaryV1 {
        CodeGraphSymbolSummaryV1 {
            occurrence: id(occurrence),
            binding: Some(CodeGraphSymbolBindingV1 {
                file: id("file.main"),
                logical_path: Some(FILE.to_owned()),
                source_span: Some(SourceSpan {
                    start_byte: u64::from(start_line),
                    end_byte: u64::from(start_line) + 1,
                }),
                chunk: None,
                language_descriptor_revision: id("language.rust.v1"),
            }),
            metadata: Some(LineageSymbolRecordV1 {
                occurrence: id(occurrence),
                identity: digest('a'),
                qualified_name: format!("crate::{name}"),
                simple_name: name.to_owned(),
                kind: kind.to_owned(),
                visibility: "private".to_owned(),
                branches: 0,
                loops: 0,
                max_nesting: 0,
                line_span,
                start_line,
                signature: signature.map(str::to_owned),
                skip_test_coverage: false,
                file_identity: digest('e'),
                content_digest: digest('d'),
            }),
        }
    }

    /// The fixture file's symbols: `helper` on 1-based lines 2-3 and `main` on
    /// 1-based lines 5-8.
    fn fixture() -> Vec<CodeGraphSymbolSummaryV1> {
        vec![
            symbol(
                "sym.helper",
                "helper",
                "function",
                1,
                2,
                Some("fn helper() -> String"),
            ),
            symbol("sym.main", "main", "function", 4, 4, Some("fn main()")),
        ]
    }

    #[test]
    fn symbol_context_scopes_to_the_requested_line_range() {
        let value = symbol_context_value(&fixture(), FILE, LineRange::parse("5-7"));

        assert_eq!(value["range"], json!({"start": 5, "end": 7}));
        assert_eq!(value["symbol_count"], 1);
        assert_eq!(value["truncated"], false);
        assert!(
            value.get("note").is_none(),
            "unexpected degradation: {value}"
        );
        let symbols = value["symbols"].as_array().expect("symbol array");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0]["name"], "main");
        assert_eq!(symbols[0]["line"], 5);
        assert_eq!(symbols[0]["end_line"], 8);
        assert_eq!(symbols[0]["signature"], "fn main()");
    }

    #[test]
    fn symbol_context_keeps_symbols_straddling_the_range_edges() {
        // A one-line read inside `main`'s body still reports `main`.
        let value = symbol_context_value(&fixture(), FILE, LineRange::parse("6"));
        let names = value["symbols"]
            .as_array()
            .expect("symbol array")
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["main".to_owned()]);
    }

    #[test]
    fn symbol_context_without_a_range_reports_the_whole_file_nearest_first() {
        let value = symbol_context_value(&fixture(), FILE, None);

        assert_eq!(value["range"], Value::Null);
        assert_eq!(value["symbol_count"], 2);
        assert_eq!(value["symbols"][0]["name"], "helper");
        assert_eq!(value["symbols"][1]["name"], "main");
    }

    #[test]
    fn symbol_context_degrades_instead_of_failing_when_no_span_is_published() {
        // `line_span == 0` is the degenerate record: it must not scope, and it
        // must not fail the read that carries this enrichment.
        let nodes = vec![symbol("sym.opaque", "opaque", "function", 0, 0, None)];
        let value = symbol_context_value(&nodes, FILE, LineRange::parse("5-7"));

        assert_eq!(value["range"], Value::Null);
        assert_eq!(value["symbol_count"], 1);
        assert_eq!(value["symbols"][0]["line"], Value::Null);
        assert_eq!(value["symbols"][0]["end_line"], Value::Null);
        assert!(
            value["note"]
                .as_str()
                .is_some_and(|note| note.contains("line spans are not published")),
            "expected a degradation note: {value}"
        );
    }

    #[test]
    fn signatures_publish_the_projection_signature_text() {
        let value = signatures_value(&fixture(), FILE);

        assert_eq!(value["file"], FILE);
        assert_eq!(value["symbol_count"], 2);
        assert_eq!(value["without_signature"], 0);
        assert_eq!(value["symbols"][0]["name"], "helper");
        assert_eq!(value["symbols"][0]["kind"], "function");
        assert_eq!(value["symbols"][0]["line"], 2);
        assert_eq!(value["symbols"][0]["signature"], "fn helper() -> String");
        assert_eq!(value["symbols"][1]["signature"], "fn main()");
    }

    #[test]
    fn signatures_count_symbols_the_extractor_left_unsigned() {
        let mut nodes = fixture();
        nodes.push(symbol("sym.blank", "blank", "struct", 9, 1, Some("  ")));
        nodes.push(symbol("sym.none", "none", "struct", 11, 1, None));

        let value = signatures_value(&nodes, FILE);

        assert_eq!(value["symbol_count"], 2);
        assert_eq!(value["without_signature"], 2);
    }

    #[test]
    fn map_keeps_byte_spans_and_filters_by_kind() {
        let mut nodes = fixture();
        nodes.push(symbol("sym.holder", "Holder", "struct", 9, 1, None));

        let all = map_value(&nodes, FILE, None);
        assert_eq!(all["symbol_count"], 3);
        assert_eq!(all["symbols"][0]["start_byte"], 1);
        assert_eq!(all["symbols"][0]["qualified_name"], "crate::helper");

        let structs = map_value(&nodes, FILE, Some(&["Struct".to_owned()]));
        assert_eq!(structs["symbol_count"], 1);
        assert_eq!(structs["symbols"][0]["name"], "Holder");
    }
}
