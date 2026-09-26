//! Blocks every surface appends beside a rendered tool result: the stale
//! code-graph trailer, the request cost receipt, and the token-accounting
//! footer.
//!
//! MCP and the `tracedecay tool` CLI render through the same functions, so
//! both print the same trailer and footer for the same typed result.

use std::path::{Component, Path};

use serde_json::json;
use tracedecay_contracts::RequestCostReceiptV1;
use tracedecay_contracts::retrieval::{CodeGraphReadFreshnessV1, ServedCodeGraphGenerationV1};

use super::ToolResult;

pub const TOKEN_ACCOUNTING_FOOTER_PREFIX: &str = "tracedecay_metrics:";
pub const CODE_GRAPH_FRESHNESS_TRAILER_PREFIX: &str = "code_graph_freshness:";
pub const REQUEST_COST_TRAILER_PREFIX: &str = "tracedecay_cost:";

/// Token estimate for one rendered result: reading its touched files raw
/// versus the response it actually delivered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolTokenAccounting {
    pub raw_file_tokens: u64,
    pub response_tokens: u64,
}

impl ToolTokenAccounting {
    pub fn net_saved_tokens(self) -> u64 {
        self.raw_file_tokens.saturating_sub(self.response_tokens)
    }
}

/// What a typed envelope carries beside its body. Every typed renderer
/// attaches it through [`ResponseTrailer::attach`], so each surface prints the
/// same trailer and footer for the same envelope.
#[derive(Clone, Copy, Debug)]
pub struct ResponseTrailer<'a> {
    /// Project files the answer read; the token-accounting footer prices
    /// reading them raw.
    pub touched_files: &'a [String],
    /// The generation a code-graph read served; a stale seat adds the
    /// `code_graph_freshness` trailer.
    pub code_graph: Option<&'a ServedCodeGraphGenerationV1>,
    /// What the read cost its stores; adds the `tracedecay_cost` trailer.
    pub cost: Option<&'a RequestCostReceiptV1>,
}

impl ResponseTrailer<'_> {
    pub fn attach(self, result: &mut ToolResult) {
        result.touched_files = self.touched_files.to_vec();
        if let Some(served) = self.code_graph {
            append_code_graph_freshness(result, served);
        }
        if let Some(cost) = self.cost {
            append_request_cost(result, cost);
        }
    }
}

/// Records `cost` on the result and appends its `tracedecay_cost` trailer.
pub fn append_request_cost(result: &mut ToolResult, cost: &RequestCostReceiptV1) {
    result.cost = Some(*cost);
    let Some(content) = result
        .value
        .get_mut("content")
        .and_then(|content| content.as_array_mut())
    else {
        return;
    };
    let RequestCostReceiptV1 {
        wall_micros,
        point_reads,
        adjacency_queries,
        adjacency_rows,
        bytes_hydrated,
    } = cost;
    content.push(json!({"type": "text", "text": format!(
        "\n{REQUEST_COST_TRAILER_PREFIX} wall_us={wall_micros} graph_sealed_reads={} \
         graph_staging_reads={} adjacency_queries={adjacency_queries} \
         adjacency_rows={adjacency_rows} bytes_hydrated={bytes_hydrated}",
        point_reads.graph_sealed, point_reads.graph_staging,
    )}));
}

/// Appends the `code_graph_freshness` trailer when `served` is a stale seat.
pub fn append_code_graph_freshness(result: &mut ToolResult, served: &ServedCodeGraphGenerationV1) {
    let CodeGraphReadFreshnessV1::LastCompleteStale {
        sealed_at,
        rebuild_in_flight,
    } = served.freshness
    else {
        return;
    };
    let Some(content) = result
        .value
        .get_mut("content")
        .and_then(|content| content.as_array_mut())
    else {
        return;
    };
    let generation = &served.generation;
    let age = seated_generation_age_label(sealed_at);
    let remedy = if rebuild_in_flight {
        "while the code index rebuilds"
    } else {
        "while source freshness remains unverified"
    };
    content.push(json!({"type": "text", "text": format!(
        "\n{CODE_GRAPH_FRESHNESS_TRAILER_PREFIX} stale, serving the last complete generation \
         {generation} (sealed {age} ago) {remedy}; results may trail the live worktree"
    )}));
}

/// Coarse human duration between a generation's seal time and now. A routine
/// rebuild window reads in seconds or minutes; a wedged route in hours or days.
fn seated_generation_age_label(sealed_at: tracedecay_domain::UtcMicros) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(sealed_at.0, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        });
    let seconds = now.saturating_sub(sealed_at.0).max(0) / 1_000_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// Approximate tokens (chars / 4) across the result's text blocks.
pub fn response_token_count(result: &ToolResult) -> u64 {
    let chars: usize = result
        .value
        .get("content")
        .and_then(|content| content.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
        .map(str::len)
        .sum();
    (chars / 4) as u64
}

/// Raw-read counterfactual: every touched project file read in full.
/// Absolute or escaping paths are not project files and cost nothing.
pub fn raw_file_tokens(project_root: &Path, touched_files: &[String]) -> u64 {
    touched_files
        .iter()
        .filter(|path| !path.is_empty())
        .filter_map(|path| {
            let relative = Path::new(path);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
            {
                return None;
            }
            std::fs::metadata(project_root.join(relative))
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map(|metadata| metadata.len() / 4)
        })
        .fold(0, u64::saturating_add)
}

/// Accounts a rendered result's tokens, records the figures on the result, and
/// appends the footer when the touched files cost anything to read raw.
pub fn account_tool_result(project_root: Option<&Path>, result: &mut ToolResult) {
    let accounting = ToolTokenAccounting {
        raw_file_tokens: project_root
            .map_or(0, |root| raw_file_tokens(root, &result.touched_files)),
        response_tokens: response_token_count(result),
    };
    record_token_accounting(result, accounting);
}

/// Records `accounting` on the result and appends its footer when the
/// touched files cost anything to read raw.
pub fn record_token_accounting(result: &mut ToolResult, accounting: ToolTokenAccounting) {
    let ToolTokenAccounting {
        raw_file_tokens,
        response_tokens,
    } = accounting;
    if raw_file_tokens > 0
        && let Some(content) = result
            .value
            .get_mut("content")
            .and_then(|content| content.as_array_mut())
    {
        content.push(json!({"type": "text", "text": format!(
            "\n{TOKEN_ACCOUNTING_FOOTER_PREFIX} before={raw_file_tokens} after={response_tokens}"
        )}));
    }
    result.set_token_accounting(accounting);
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::UtcMicros;

    use super::*;

    fn text_result(text: &str, touched: Vec<String>) -> ToolResult {
        ToolResult::new(
            json!({"content": [{"type": "text", "text": text}]}),
            touched,
        )
    }

    fn block(result: &ToolResult, index: usize) -> Option<&str> {
        result.value["content"][index]["text"].as_str()
    }

    #[test]
    fn stale_seat_appends_the_trailer_and_a_current_seat_does_not() {
        let mut stale = text_result("{}", Vec::new());
        append_code_graph_freshness(
            &mut stale,
            &ServedCodeGraphGenerationV1 {
                generation: "generation.fixture.7".to_owned(),
                freshness: CodeGraphReadFreshnessV1::LastCompleteStale {
                    sealed_at: UtcMicros(0),
                    rebuild_in_flight: true,
                },
            },
        );
        let trailer = block(&stale, 1).expect("stale trailer");
        assert!(
            trailer.starts_with(
                "\ncode_graph_freshness: stale, serving the last complete generation \
                 generation.fixture.7 (sealed "
            ),
            "{trailer}"
        );
        assert!(
            trailer.ends_with(
                "ago) while the code index rebuilds; results may trail the live worktree"
            ),
            "{trailer}"
        );

        let mut current = text_result("{}", Vec::new());
        append_code_graph_freshness(
            &mut current,
            &ServedCodeGraphGenerationV1 {
                generation: "generation.fixture.8".to_owned(),
                freshness: CodeGraphReadFreshnessV1::Current,
            },
        );
        assert_eq!(current.value["content"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn a_metered_read_renders_its_cost_trailer_and_an_unmetered_one_does_not() {
        let cost = RequestCostReceiptV1 {
            wall_micros: 1_250,
            point_reads: tracedecay_contracts::StorePointReadsV1 {
                graph_sealed: 21,
                graph_staging: 3,
            },
            adjacency_queries: 1,
            adjacency_rows: 107,
            bytes_hydrated: 9_876,
        };
        let mut metered = text_result("{}", Vec::new());
        ResponseTrailer {
            touched_files: &[],
            code_graph: None,
            cost: Some(&cost),
        }
        .attach(&mut metered);
        assert_eq!(
            block(&metered, 1),
            Some(
                "\ntracedecay_cost: wall_us=1250 graph_sealed_reads=21 graph_staging_reads=3 \
                 adjacency_queries=1 adjacency_rows=107 bytes_hydrated=9876"
            )
        );
        assert_eq!(metered.cost(), Some(cost));

        let mut unmetered = text_result("{}", Vec::new());
        ResponseTrailer {
            touched_files: &[],
            code_graph: None,
            cost: None,
        }
        .attach(&mut unmetered);
        assert_eq!(unmetered.value["content"].as_array().map(Vec::len), Some(1));
        assert_eq!(unmetered.cost(), None);
    }

    #[test]
    fn accounting_footer_counts_touched_files_and_the_response() {
        let root = tempfile::tempdir().expect("root");
        std::fs::write(root.path().join("lib.rs"), "x".repeat(400)).expect("source");
        let mut result = text_result(&"y".repeat(40), vec!["lib.rs".to_owned()]);
        account_tool_result(Some(root.path()), &mut result);
        assert_eq!(
            block(&result, 1),
            Some("\ntracedecay_metrics: before=100 after=10")
        );
        assert_eq!(
            result.token_accounting(),
            Some(ToolTokenAccounting {
                raw_file_tokens: 100,
                response_tokens: 10,
            })
        );
    }

    #[test]
    fn untouched_or_escaping_files_add_no_footer() {
        let root = tempfile::tempdir().expect("root");
        let project = root.path().join("project");
        std::fs::create_dir(&project).expect("project");
        let outside = root.path().join("outside.rs");
        std::fs::write(&outside, "x".repeat(400)).expect("outside source");
        let mut result = text_result(
            "body",
            vec![
                "../outside.rs".to_owned(),
                outside.display().to_string(),
                "missing.rs".to_owned(),
            ],
        );
        account_tool_result(Some(&project), &mut result);
        assert_eq!(result.value["content"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            result.token_accounting(),
            Some(ToolTokenAccounting {
                raw_file_tokens: 0,
                response_tokens: 1,
            })
        );
    }
}
