use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

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
    Ok(json!({"file": file_path, "symbol_count": symbols.len(), "symbols": symbols}))
}

pub fn render_signatures(
    _reader: &CodeGraphInteractiveReader,
    _cancellation: Arc<dyn GraphCancellation>,
    file_path: &str,
) -> Result<Value> {
    Err(unavailable(&format!(
        "signature text is not published for {file_path} in the verified graph projection"
    )))
}

pub fn render_symbol_context(
    reader: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    file_path: &str,
    range: Option<LineRange>,
) -> Result<Value> {
    if range.is_some() {
        return Err(unavailable(
            "line-range symbol context requires a line/byte map that the verified graph projection does not publish",
        ));
    }
    let nodes = fetch_nodes(reader, cancellation, file_path)?;
    let symbols = nodes
        .iter()
        .take(MAX_CONTEXT_SYMBOLS)
        .map(symbol_entry)
        .collect::<Vec<_>>();
    Ok(
        json!({"file": file_path, "range": Value::Null, "symbol_count": nodes.len(), "truncated": nodes.len() > symbols.len(), "symbols": symbols}),
    )
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

fn symbol_entry(node: &CodeGraphSymbolSummaryV1) -> Value {
    let span = node
        .binding
        .as_ref()
        .and_then(|binding| binding.source_span);
    json!({
        "id": node.occurrence.as_str(),
        "kind": node.metadata.as_ref().map(|metadata| metadata.kind.as_str()),
        "qualified_name": node.metadata.as_ref().map(|metadata| metadata.qualified_name.as_str()),
        "start_byte": span.map(|span| span.start_byte),
        "end_byte": span.map(|span| span.end_byte),
    })
}

fn unavailable(detail: &str) -> TraceDecayError {
    TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-evidence-unavailable".to_owned(),
        retryable: false,
        detail: detail.to_owned(),
    }
}
