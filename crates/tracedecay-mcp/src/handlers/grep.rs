//! Content-search tool handler: `tracedecay_grep`.
//!
//! Literal/regex search over UTF-8 text sources in the project working tree
//! (respecting `.gitignore`). This closes the gap that made agents fall back
//! to raw `rg`.
//! `tracedecay_search` only matches symbol *names*, not file *content*.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_code_index::grep_search::{
    GrepScanOmissionsV1, GrepSearchHit, GrepSearchQuery, MAX_INTERACTIVE_SOURCE_BYTES,
    MAX_LINE_BYTES, search_tree_with_cancel,
};
use tracedecay_contracts::{
    CoverageCompleteness, CoverageDomainState, EvidenceCoverage, EvidenceDomain, Omission,
    OmissionReason,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

use crate::ToolResult;
use crate::handlers::run_bounded_search;
use crate::tools::render::{self, Md};
use crate::unique_file_paths;

/// Hard cap on `max_results` regardless of what the caller requests.
const MAX_RESULTS_CAP: usize = 200;
/// Default `max_results` when the caller omits it.
const DEFAULT_MAX_RESULTS: usize = 50;
/// Hard cap on `context_lines`.
const MAX_CONTEXT_LINES: usize = 3;
/// Maximum graph symbols inspected while enriching one bounded grep response.
const MAX_ENRICHMENT_SYMBOLS: usize = 500_000;
/// A single bounded content-search hit.
struct GrepHit {
    file: String,
    line: u32,
    text: String,
    before: Vec<String>,
    after: Vec<String>,
    symbol: Option<String>,
    node_id: Option<String>,
}

impl From<GrepSearchHit> for GrepHit {
    fn from(hit: GrepSearchHit) -> Self {
        Self {
            file: hit.file.to_string(),
            line: hit.line,
            text: hit.text,
            before: hit.before,
            after: hit.after,
            symbol: None,
            node_id: None,
        }
    }
}

#[hotpath::measure(future = true, label = "mcp.search.grep.total")]
pub async fn handle_grep(
    project_root: &Path,
    response_handle_root: &Path,
    graph: std::result::Result<&VerifiedGraphQuery, &TraceDecayError>,
    args: Value,
    scope_prefix: Option<&str>,
    deadline: Option<tracedecay_contracts::Deadline>,
    cancellation: Option<tracedecay_contracts::CancellationSignal>,
) -> Result<ToolResult> {
    let pattern =
        args.get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: pattern".to_string(),
            })?;
    if pattern.is_empty() {
        return Err(TraceDecayError::Config {
            message: "pattern must not be empty".to_string(),
        });
    }

    let fixed_strings = args
        .get("fixed_strings")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let path_glob = args
        .get("path_glob")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_RESULTS, |v| (v as usize).min(MAX_RESULTS_CAP))
        .max(1);
    let context_lines = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .map_or(0, |v| (v as usize).min(MAX_CONTEXT_LINES));

    let project_root_buf = project_root.to_path_buf();
    let query = GrepSearchQuery {
        pattern: pattern.to_owned(),
        fixed_strings,
        case_sensitive,
        path_glob,
        context_lines,
        max_results,
    };
    let scan = hotpath::future!(
        run_bounded_search(
            "tracedecay_grep",
            pattern.to_owned(),
            deadline,
            cancellation,
            move |cancelled, transport_cancellation| {
                search_tree_with_cancel(&project_root_buf, &query, || {
                    cancelled.load(std::sync::atomic::Ordering::Acquire)
                        || transport_cancellation
                            .as_ref()
                            .is_some_and(tracedecay_contracts::CancellationSignal::is_cancelled)
                })
            },
        ),
        label = "mcp.search.grep.scan"
    )
    .await?;

    // Scope filtering mirrors `tracedecay_search`: when the client pins a
    // subtree, only hits under it are returned.
    let mut hits = scan
        .hits
        .into_iter()
        .map(GrepHit::from)
        .filter(|hit| tracedecay_domain::path_matches_scope(hit.file.as_str(), scope_prefix))
        .collect::<Vec<_>>();
    let truncated = scan.truncated || hits.len() > max_results;
    hits.truncate(max_results);

    // Enrichment is the same optional accelerator as the open itself: a
    // published occurrence graph still refuses catalog-backed lookups while
    // its interactive catalog warms, and that refusal is retryable state the
    // one-shot caller never re-sends. Report it beside the lexical answer.
    let enrichment_error = match graph {
        Ok(graph) => enrich_hits_from_graph(graph, &mut hits).err(),
        Err(_) => None,
    };
    let graph_error = enrichment_error.as_ref().or_else(|| graph.err());
    let touched_files = unique_file_paths(hits.iter().map(|hit| hit.file.as_str()));
    let mut output_value = build_output_value(
        &hits,
        truncated,
        scan.files_scanned,
        scan.lines_examined,
        scan.omissions,
    );
    output_value["graph_enrichment"] = graph_enrichment_value(&hits, graph_error);

    let text = hotpath::measure_block!(
        "mcp.search.grep.render",
        render::finalize(Some(response_handle_root), &args, &output_value, || {
            render_grep_md(&hits, truncated, scan.files_scanned, scan.omissions)
        })
    );
    // Grep aggregates more raw content than any other search tool; the encoded
    // payload size explains transport pressure that timing alone cannot.
    hotpath::gauge!("mcp.search.grep.response_bytes").set(text.len());
    Ok(ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    ))
}

#[hotpath::measure]
fn enrich_hits_from_graph(graph: &VerifiedGraphQuery, hits: &mut [GrepHit]) -> Result<()> {
    let paths = hits
        .iter()
        .map(|hit| hit.file.clone())
        .collect::<HashSet<_>>();
    let page = graph.symbols_in_logical_files_page(
        &paths,
        None,
        MAX_ENRICHMENT_SYMBOLS,
        MAX_ENRICHMENT_SYMBOLS,
    )?;
    if page.has_more {
        return Err(TraceDecayError::project_route(
            "verified-grep-enrichment-budget-exhausted",
            false,
            "grep hit enrichment exceeded its verified graph symbol budget",
        ));
    }
    let mut symbols_by_file = HashMap::<String, Vec<CodeGraphSymbolSummaryV1>>::new();
    for symbol in page.symbols {
        if let Some(path) = symbol
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_ref())
        {
            symbols_by_file
                .entry(path.clone())
                .or_default()
                .push(symbol);
        }
    }
    for hit in hits {
        let Some(symbols) = symbols_by_file.get(&hit.file) else {
            continue;
        };
        let enclosing = symbols
            .iter()
            .filter_map(|symbol| {
                let metadata = symbol.metadata.as_ref()?;
                let start = metadata.start_line.checked_add(1)?;
                let end = start.checked_add(metadata.line_span.checked_sub(1)?)?;
                (start <= hit.line && hit.line <= end).then_some((symbol, metadata))
            })
            .min_by(|(left, left_metadata), (right, right_metadata)| {
                left_metadata
                    .line_span
                    .cmp(&right_metadata.line_span)
                    .then_with(|| right_metadata.start_line.cmp(&left_metadata.start_line))
                    .then_with(|| left.occurrence.cmp(&right.occurrence))
            });
        if let Some((symbol, metadata)) = enclosing {
            hit.symbol = Some(metadata.simple_name.clone());
            hit.node_id = Some(symbol.occurrence.as_str().to_owned());
        }
    }
    Ok(())
}

#[hotpath::measure]
fn graph_enrichment_value(hits: &[GrepHit], error: Option<&TraceDecayError>) -> Value {
    if let Some(error) = error {
        return match error.project_route_context() {
            Some((reason_code, retryable, detail)) => json!({
                "status": "unavailable",
                "reason_code": reason_code,
                "retryable": retryable,
                "detail": detail,
            }),
            None => json!({
                "status": "unavailable",
                "reason_code": "verified-code-graph-read-unavailable",
                "retryable": false,
                "detail": error.to_string(),
            }),
        };
    }
    json!({
        "status": "complete",
        "enriched": hits.iter().filter(|hit| hit.node_id.is_some()).count(),
        "returned": hits.len(),
    })
}

#[hotpath::measure]
fn grep_metadata(
    lines_examined: usize,
    returned: usize,
    truncated: bool,
    omission_counts: GrepScanOmissionsV1,
) -> (EvidenceCoverage, Vec<Omission>) {
    let completeness = if truncated || omission_counts.any() {
        CoverageCompleteness::Partial
    } else {
        CoverageCompleteness::Complete
    };
    let visited = lines_examined as u64;
    let returned = returned as u64;
    let coverage = EvidenceCoverage {
        requested_domains: vec![EvidenceDomain::Source],
        // Coverage counts use matching lines as the eligible/returned grain.
        // `visited` is every bounded text line actually tested by the matcher;
        // any omission makes the total eligible matches unknown.
        visited: Some(visited),
        eligible: (completeness == CoverageCompleteness::Complete).then_some(returned),
        returned,
        completeness,
        domains: vec![CoverageDomainState {
            domain: EvidenceDomain::Source,
            completeness,
        }],
    };
    let mut omissions = Vec::with_capacity(2);
    let budget_omissions = omission_counts.budget();
    if budget_omissions > 0 {
        omissions.push(Omission {
            domain: EvidenceDomain::Source,
            count: budget_omissions as u64,
            reason: OmissionReason::Budget,
        });
    }
    if omission_counts.unavailable_sources > 0 {
        omissions.push(Omission {
            domain: EvidenceDomain::Source,
            count: omission_counts.unavailable_sources as u64,
            reason: OmissionReason::Unavailable,
        });
    }
    (coverage, omissions)
}

fn build_output_value(
    hits: &[GrepHit],
    truncated: bool,
    files_scanned: usize,
    lines_examined: usize,
    omission_counts: GrepScanOmissionsV1,
) -> Value {
    let items: Vec<Value> = hits
        .iter()
        .map(|hit| {
            let mut item = json!({
                "file": hit.file,
                "line": hit.line,
                "text": hit.text,
            });
            if !hit.before.is_empty() {
                item["before"] = json!(hit.before);
            }
            if !hit.after.is_empty() {
                item["after"] = json!(hit.after);
            }
            if let Some(symbol) = &hit.symbol {
                item["symbol"] = json!(symbol);
            }
            if let Some(node_id) = &hit.node_id {
                item["node_id"] = json!(node_id);
            }
            item
        })
        .collect();
    let (coverage, omissions) =
        grep_metadata(lines_examined, hits.len(), truncated, omission_counts);

    json!({
        "results": items,
        "match_count": hits.len(),
        "files_scanned": files_scanned,
        "truncated": truncated,
        "coverage": coverage,
        "omissions": omissions,
    })
}

fn render_grep_md(
    hits: &[GrepHit],
    truncated: bool,
    files_scanned: usize,
    omission_counts: GrepScanOmissionsV1,
) -> String {
    let mut md = Md::new();
    md.heading(2, "Grep Results");
    if hits.is_empty() {
        md.empty_note("No matching lines.");
        md.line(&format!("_Scanned {files_scanned} files._"));
        append_partial_coverage_md(&mut md, omission_counts);
        return md.render();
    }

    for hit in hits {
        let location = format!("{}:{}", hit.file, hit.line);
        md.bullet(&location);
        for line in &hit.before {
            md.line(&format!("    {line}"));
        }
        md.line(&format!("  > {}", hit.text));
        for line in &hit.after {
            md.line(&format!("    {line}"));
        }
        if let (Some(symbol), Some(node_id)) = (&hit.symbol, &hit.node_id) {
            md.line(&format!("  _Enclosing symbol: `{symbol}` (`{node_id}`)_"));
        }
    }
    if hits.iter().any(|hit| hit.node_id.is_some()) {
        md.blank();
        md.line(
            "_Use `tracedecay_source_body` with a result's `node_id` to read the verified enclosing symbol._",
        );
    }

    md.blank();
    let mut summary = format!("_{} matches across {files_scanned} files._", hits.len());
    if truncated {
        let _ = write!(
            summary,
            " Results capped. Narrow with `path_glob` or a more specific pattern."
        );
    }
    md.line(&summary);
    append_partial_coverage_md(&mut md, omission_counts);
    md.render()
}

fn append_partial_coverage_md(md: &mut Md, omissions: GrepScanOmissionsV1) {
    if omissions.oversized_files > 0 {
        let noun = if omissions.oversized_files == 1 {
            "file"
        } else {
            "files"
        };
        md.line(&format!(
            "_Coverage is partial: skipped {} {noun} larger than the \
             {MAX_INTERACTIVE_SOURCE_BYTES}-byte scan limit; matching lines may be omitted._",
            omissions.oversized_files
        ));
    }
    if omissions.oversized_lines > 0 {
        let noun = if omissions.oversized_lines == 1 {
            "line"
        } else {
            "lines"
        };
        md.line(&format!(
            "_Coverage is partial: skipped {} {noun} longer than the \
             {MAX_LINE_BYTES}-byte scan limit; matching lines may be omitted._",
            omissions.oversized_lines
        ));
    }
    if omissions.unavailable_sources > 0 {
        let noun = if omissions.unavailable_sources == 1 {
            "source candidate was"
        } else {
            "source candidates were"
        };
        md.line(&format!(
            "_Coverage is partial: {} {noun} unavailable during the scan; matching lines may be \
             omitted._",
            omissions.unavailable_sources
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tracedecay_code_index::grep_search::GrepSearchResult;

    use super::*;

    fn scan(
        project: &Path,
        pattern: &str,
        path_glob: Option<&str>,
        max_results: usize,
        is_cancelled: impl Fn() -> bool,
    ) -> GrepSearchResult {
        search_tree_with_cancel(
            project,
            &GrepSearchQuery {
                pattern: pattern.to_owned(),
                fixed_strings: false,
                case_sensitive: true,
                path_glob: path_glob.map(str::to_owned),
                context_lines: 0,
                max_results,
            },
            is_cancelled,
        )
        .expect("bounded scan")
    }

    #[test]
    fn scan_tree_hits_markdown_heading_body() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::create_dir_all(project.path().join("docs/plans")).expect("docs fixture directory");
        std::fs::write(
            project.path().join("docs/plans/notes.md"),
            "# Remaining work by lane\n\nUNIQUE_MARKDOWN_HEADING_BODY_TOKEN in the section body.\n",
        )
        .expect("markdown fixture");

        let scan = scan(
            project.path(),
            "UNIQUE_MARKDOWN_HEADING_BODY_TOKEN",
            None,
            10,
            || false,
        );

        assert_eq!(scan.hits.len(), 1, "{scan:?}");
        assert_eq!(scan.hits[0].file.as_ref(), "docs/plans/notes.md");
        assert_eq!(scan.hits[0].line, 3);
        assert!(
            scan.hits[0]
                .text
                .contains("UNIQUE_MARKDOWN_HEADING_BODY_TOKEN"),
            "{scan:?}"
        );
    }

    #[test]
    fn scan_tree_prunes_generated_dependency_directories_without_gitignore() {
        let project = tempfile::tempdir().expect("temp project");
        let generated = project.path().join(".venv/lib/python/site-packages/pkg");
        std::fs::create_dir_all(&generated).expect("generated fixture directory");
        std::fs::create_dir_all(project.path().join("src")).expect("source fixture directory");
        std::fs::create_dir_all(project.path().join(".tracedecay"))
            .expect("metadata fixture directory");
        std::fs::write(
            generated.join("generated.py"),
            "UNIQUE_GENERATED_DIR_TOKEN\n",
        )
        .expect("generated fixture");
        std::fs::write(
            project.path().join("src/tracked.rs"),
            "// UNIQUE_GENERATED_DIR_TOKEN\n",
        )
        .expect("source fixture");
        std::fs::write(
            project.path().join(".git"),
            "gitdir: UNIQUE_GENERATED_DIR_TOKEN\n",
        )
        .expect("linked-worktree git file fixture");
        std::fs::write(
            project.path().join(".tracedecay/internal.txt"),
            "UNIQUE_GENERATED_DIR_TOKEN\n",
        )
        .expect("metadata fixture");

        let scan = scan(
            project.path(),
            "UNIQUE_GENERATED_DIR_TOKEN",
            None,
            10,
            || false,
        );
        let files = scan
            .hits
            .iter()
            .map(|hit| hit.file.as_ref())
            .collect::<Vec<_>>();

        assert!(files.contains(&"src/tracked.rs"), "{files:?}");
        assert!(
            !files.iter().any(|file| file.starts_with(".venv/")),
            "generated dependency trees must be pruned: {files:?}"
        );
        assert!(
            !files.contains(&".git"),
            "git metadata must be pruned: {files:?}"
        );
        assert!(
            !files.iter().any(|file| file.starts_with(".tracedecay/")),
            "TraceDecay metadata must be pruned: {files:?}"
        );
    }

    #[test]
    fn scan_tree_path_glob_prunes_unrelated_generated_directories() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::create_dir_all(project.path().join("src")).expect("source fixture directory");
        std::fs::write(
            project.path().join("src/selected.rs"),
            "NORMAL_PATH_GLOB_TOKEN\n",
        )
        .expect("source fixture");

        let baseline_checks = std::sync::atomic::AtomicUsize::new(0);
        let baseline = scan(
            project.path(),
            "NORMAL_PATH_GLOB_TOKEN",
            Some("src/**"),
            10,
            || {
                baseline_checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            },
        );

        std::fs::create_dir_all(project.path().join("target/generated"))
            .expect("generated fixture directory");
        std::fs::write(
            project.path().join("target/generated/unrelated.rs"),
            "NORMAL_PATH_GLOB_TOKEN\n",
        )
        .expect("generated fixture");

        let checks = std::sync::atomic::AtomicUsize::new(0);
        let scan = scan(
            project.path(),
            "NORMAL_PATH_GLOB_TOKEN",
            Some("src/**"),
            10,
            || {
                checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            },
        );

        assert_eq!(scan.hits.len(), baseline.hits.len());
        assert_eq!(
            checks.load(std::sync::atomic::Ordering::Relaxed),
            baseline_checks.load(std::sync::atomic::Ordering::Relaxed),
            "unrelated generated directories must not reach the scan loop"
        );
    }

    #[test]
    fn scan_tree_slashless_glob_includes_generated_directory_descendants() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::create_dir_all(project.path().join("dist")).expect("generated fixture directory");
        std::fs::write(
            project.path().join("dist/generated.js"),
            "SLASHLESS_GENERATED_GLOB_TOKEN\n",
        )
        .expect("generated fixture");

        let scan = scan(
            project.path(),
            "SLASHLESS_GENERATED_GLOB_TOKEN",
            Some("*.js"),
            10,
            || false,
        );

        let files = scan
            .hits
            .iter()
            .map(|hit| hit.file.as_ref())
            .collect::<Vec<_>>();
        assert!(
            files.contains(&"dist/generated.js"),
            "slashless basename globs must match generated descendants: {files:?}"
        );
    }

    fn write_oversized_match(path: &Path, pattern: &str) {
        let mut source = format!("{pattern}\n").into_bytes();
        source.resize((MAX_INTERACTIVE_SOURCE_BYTES as usize) + 1, b'x');
        std::fs::write(path, source).expect("oversized fixture");
    }

    fn assert_partial_output(output: &Value, visited: u64, returned: u64, omissions: &Value) {
        assert_eq!(output["truncated"], json!(false));
        assert_eq!(output["coverage"]["completeness"], json!("partial"));
        assert_eq!(output["coverage"]["visited"], json!(visited));
        assert_eq!(output["coverage"]["eligible"], Value::Null);
        assert_eq!(output["coverage"]["returned"], json!(returned));
        assert_eq!(&output["omissions"], omissions);
    }

    fn scan_output(project: &Path, pattern: &str) -> (GrepSearchResult, Value) {
        let scan = scan(project, pattern, None, 10, || false);
        let hits = scan
            .hits
            .iter()
            .cloned()
            .map(GrepHit::from)
            .collect::<Vec<_>>();
        let output = build_output_value(
            &hits,
            scan.truncated,
            scan.files_scanned,
            scan.lines_examined,
            scan.omissions,
        );
        (scan, output)
    }

    fn rendered_scan(scan: &GrepSearchResult) -> String {
        let hits = scan
            .hits
            .iter()
            .cloned()
            .map(GrepHit::from)
            .collect::<Vec<_>>();
        render_grep_md(&hits, scan.truncated, scan.files_scanned, scan.omissions)
    }

    fn one_budget_omission() -> Value {
        json!([{"domain": "source", "count": 1, "reason": "budget"}])
    }

    #[test]
    fn matching_oversized_file_reports_partial_coverage_without_result_truncation() {
        let project = tempfile::tempdir().expect("temp project");
        write_oversized_match(
            &project.path().join("oversized.txt"),
            "OVERSIZED_ONLY_TOKEN",
        );

        let (scan, output) = scan_output(project.path(), "OVERSIZED_ONLY_TOKEN");

        assert!(scan.hits.is_empty());
        assert_eq!(scan.omissions.oversized_files, 1);
        assert!(!scan.truncated);
        assert_partial_output(&output, 0, 0, &one_budget_omission());

        let markdown = rendered_scan(&scan);
        assert!(markdown.contains("No matching lines."), "{markdown}");
        assert!(
            markdown.contains(&format!(
                "skipped 1 file larger than the {MAX_INTERACTIVE_SOURCE_BYTES}-byte scan limit"
            )),
            "{markdown}"
        );
    }

    #[test]
    fn mixed_ordinary_and_oversized_files_return_hits_with_partial_coverage() {
        let project = tempfile::tempdir().expect("temp project");
        write_oversized_match(
            &project.path().join("oversized.txt"),
            "MIXED_OVERSIZED_TOKEN",
        );
        std::fs::write(
            project.path().join("tracked.txt"),
            "ordinary prefix\nMIXED_OVERSIZED_TOKEN\nordinary suffix\n",
        )
        .expect("source fixture");

        let (scan, output) = scan_output(project.path(), "MIXED_OVERSIZED_TOKEN");

        assert_eq!(scan.hits.len(), 1);
        assert_eq!(scan.hits[0].file.as_ref(), "tracked.txt");
        assert_eq!(scan.files_scanned, 1);
        assert_eq!(scan.lines_examined, 3);
        assert_eq!(scan.omissions.oversized_files, 1);
        assert!(!scan.truncated);
        assert_eq!(output["results"][0]["file"], json!("tracked.txt"));
        assert_partial_output(&output, 3, 1, &one_budget_omission());

        let markdown = rendered_scan(&scan);
        assert!(markdown.contains("tracked.txt:2"), "{markdown}");
        assert!(markdown.contains("Coverage is partial"), "{markdown}");
    }

    #[test]
    fn overlong_matching_line_is_a_budget_omission() {
        let project = tempfile::tempdir().expect("temp project");
        let long_line = format!("OVERLONG_LINE_TOKEN{}", "x".repeat(MAX_LINE_BYTES));
        std::fs::write(
            project.path().join("overlong.txt"),
            format!("ordinary line\n{long_line}\n"),
        )
        .expect("overlong fixture");

        let (scan, output) = scan_output(project.path(), "OVERLONG_LINE_TOKEN");

        assert_eq!(scan.lines_examined, 1);
        assert_eq!(scan.omissions.oversized_lines, 1);
        assert_partial_output(&output, 1, 0, &one_budget_omission());

        let markdown = rendered_scan(&scan);
        assert!(
            markdown.contains(&format!(
                "skipped 1 line longer than the {MAX_LINE_BYTES}-byte scan limit"
            )),
            "{markdown}"
        );
    }

    #[test]
    fn unavailable_source_candidates_are_typed_and_ordered_after_budget_omissions() {
        let omission_counts = GrepScanOmissionsV1 {
            oversized_lines: 1,
            unavailable_sources: 2,
            ..GrepScanOmissionsV1::default()
        };
        let output = build_output_value(&[], false, 0, 0, omission_counts);

        assert_partial_output(
            &output,
            0,
            0,
            &json!([
                {
                    "domain": "source",
                    "count": 1,
                    "reason": "budget",
                },
                {
                    "domain": "source",
                    "count": 2,
                    "reason": "unavailable",
                }
            ]),
        );

        let markdown = render_grep_md(&[], false, 0, omission_counts);
        assert!(
            markdown.contains(&format!(
                "skipped 1 line longer than the {MAX_LINE_BYTES}-byte scan limit"
            )),
            "{markdown}"
        );
        assert!(
            markdown.contains("2 source candidates were unavailable"),
            "{markdown}"
        );
    }

    const ENRICHMENT_TOKEN: &str = "GREP_ENRICHMENT_REFUSAL_TOKEN";
    const ENRICHED_FILE: &str = "src/lib.rs";
    const ENRICHED_LINE: u32 = 2;
    const ENRICHED_TEXT: &str = "    let _ = \"GREP_ENRICHMENT_REFUSAL_TOKEN\";";
    const ENRICHED_SOURCE: &str =
        "pub fn greet() {\n    let _ = \"GREP_ENRICHMENT_REFUSAL_TOKEN\";\n}\n";
    const ENRICHED_SYMBOL: &str = "greet";
    const ENRICHED_OCCURRENCE: &str = "symbol.grep-enrichment.greet";

    fn fixture_id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn fixture_digest<T>(fill: char) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        fixture_id(&format!("sha256:{}", String::from(fill).repeat(64)))
    }

    /// A request context whose deadline the caller chooses. A deadline already
    /// in the past is the retryable refusal a published-but-warming graph
    /// answers catalog-backed lookups with.
    fn fixture_request_context(
        cancellation: &tracedecay_contracts::CancellationSignal,
        expires_at: tracedecay_domain::UtcMicros,
    ) -> tracedecay_contracts::RequestContext {
        use tracedecay_contracts::{
            CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext,
            RequestId, ResolvedScope,
        };
        use tracedecay_domain::{ActorId, ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId};

        let scope = ResolvedScope::new(
            fixture_id::<ProjectId>("project.grep-enrichment.fixture"),
            fixture_id::<RepositoryId>("repository.grep-enrichment.fixture"),
            fixture_id::<WorktreeId>("worktree.grep-enrichment.fixture"),
            Some(fixture_id::<RefId>("refs/heads/grep-enrichment-fixture")),
        )
        .expect("fixture resolved scope");
        let grant = CapabilityGrantSnapshot::new(
            fixture_id::<CapabilityGrantId>("grant.grep-enrichment.fixture"),
            1,
            fixture_digest('a'),
            fixture_id::<ActorId>("actor.grep-enrichment-fixture.issuer"),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            scope.clone(),
            std::collections::BTreeSet::from([
                fixture_id::<tracedecay_tool_catalog::CapabilityId>(
                    "capability.grep-enrichment.fixture",
                ),
            ]),
            std::collections::BTreeSet::from([fixture_id::<tracedecay_tool_catalog::UseCaseId>(
                "use-case.grep-enrichment.fixture",
            )]),
            DisclosureClass::Evidence,
        )
        .expect("fixture capability grant");
        RequestContext::new(
            fixture_id::<ActorId>("actor.grep-enrichment-fixture.requester"),
            scope,
            grant,
            fixture_id::<RequestId>("request.grep-enrichment.fixture"),
            Deadline::new(expires_at).expect("fixture deadline"),
            cancellation.context(),
        )
        .expect("fixture request context")
    }

    /// One published generation holding `greet`, whose line span covers the
    /// token line in [`ENRICHED_SOURCE`].
    fn fixture_graph(
        cancellation: &tracedecay_contracts::CancellationSignal,
        expires_at: tracedecay_domain::UtcMicros,
    ) -> tracedecay_graph_query::VerifiedGraphQuery {
        use std::sync::Arc;
        use tracedecay_code_index::graph_projection::HermeticCodeGraphProjectionStore;
        use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
        use tracedecay_domain::{
            BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
            CodeSearchChunkGrainV1, CodeSearchChunkV1, ComplexityAnalysisV1, ContentDigest,
            FileOccurrenceId, LanguageDescriptorRevision, LanguageId, PolicyRevisionId,
            SanitizedCodeFileV1, SanitizerRevision, SensitivityDecision, SensitivityLevelV1,
            SnapshotFileDispositionV1, SourceSpan, SymbolOccurrenceId,
        };

        let generation = fixture_id::<CodeGenerationId>("generation.grep-enrichment.1");
        let file = fixture_id::<FileOccurrenceId>("file.grep-enrichment.lib");
        let occurrence = fixture_id::<SymbolOccurrenceId>(ENRICHED_OCCURRENCE);
        let files = [SanitizedCodeFileV1 {
            file_occurrence_id: file.clone(),
            logical_path: ENRICHED_FILE.to_owned(),
            language: Some(LanguageId::new("rust").expect("fixture language")),
            content_digest: fixture_digest('b'),
            disposition: SnapshotFileDispositionV1::Present,
        }];
        let symbols = vec![Arc::new(LineageSymbolRecordV1 {
            occurrence: occurrence.clone(),
            identity: fixture_digest('c'),
            qualified_name: ENRICHED_SYMBOL.to_owned(),
            simple_name: ENRICHED_SYMBOL.to_owned(),
            kind: "function".to_owned(),
            visibility: "public".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            // `start_line` is zero-based, so this spans source lines 1 to 3.
            line_span: 3,
            start_line: 0,
            signature: None,
            docstring: None,
            is_async: false,
            derives: Vec::new(),
            skip_test_coverage: false,
            file_identity: fixture_digest('d'),
            content_digest: fixture_digest('e'),
        })];
        let chunks = [Arc::new(CodeSearchChunkV1 {
            id: fixture_id("chunk.grep-enrichment.greet"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation.clone(),
                file_occurrence_id: file,
                symbol_occurrence_id: Some(occurrence),
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: ENRICHED_SOURCE.len() as u64,
                },
                grain: CodeSearchChunkGrainV1::SymbolBody,
                ordinal: 0,
            },
            content_digest: fixture_digest::<ContentDigest>('f'),
            language_descriptor_revision: fixture_id::<LanguageDescriptorRevision>(
                "language.rust.grep-enrichment.v1",
            ),
            chunker_revision: fixture_id::<ChunkerRevision>("chunker.grep-enrichment.v1"),
            sanitizer_revision: fixture_id::<SanitizerRevision>("sanitizer.grep-enrichment.v1"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: fixture_id::<PolicyRevisionId>("policy.grep-enrichment.v1"),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new(ENRICHED_SOURCE)
                .expect("bounded fixture text"),
        })];
        let symbols = GenerationSymbolIndexV1::new(generation.clone(), symbols)
            .expect("valid fixture symbol index");

        let store =
            HermeticCodeGraphProjectionStore::memory(cancellation).expect("fixture projection");
        store
            .publish_indexed_with_cancellation(
                &generation,
                &[],
                &chunks,
                &files,
                &symbols,
                Arc::new(tracedecay_graph_db::NeverCancelled),
            )
            .expect("publish fixture generation");
        let graph_cancellation =
            tracedecay_graph_query::application_graph_cancellation(cancellation);
        let reader = store
            .verified_store(&generation)
            .expect("open verified fixture generation")
            .interactive_reader_with_cancellation(&generation, Arc::clone(&graph_cancellation))
            .expect("open generation-pinned fixture reader");
        tracedecay_graph_query::VerifiedGraphQuery::from_fixture_reader(
            reader,
            graph_cancellation,
            fixture_request_context(cancellation, expires_at),
        )
    }

    async fn grep_token(
        project: &Path,
        graph: &tracedecay_graph_query::VerifiedGraphQuery,
    ) -> Value {
        let result = handle_grep(
            project,
            &project.join("response-handles"),
            Ok(graph),
            json!({"pattern": ENRICHMENT_TOKEN, "fixed_strings": true, "format": "json"}),
            None,
            None,
            None,
        )
        .await
        .expect("grep answers lexically whatever the graph says");
        let text = result.value["content"][0]["text"]
            .as_str()
            .expect("grep json text");
        serde_json::from_str(text).expect("grep payload is JSON")
    }

    #[tokio::test]
    async fn grep_keeps_lexical_hits_when_graph_enrichment_refuses() {
        use tracedecay_domain::UtcMicros;

        let project = tempfile::tempdir().expect("temp project");
        std::fs::create_dir_all(project.path().join("src")).expect("source fixture directory");
        std::fs::write(project.path().join(ENRICHED_FILE), ENRICHED_SOURCE)
            .expect("source fixture");
        let cancellation = tracedecay_contracts::CancellationSignal::active(
            "cancellation.grep-enrichment.fixture",
        )
        .expect("fixture cancellation");
        let lexical_hit = json!({
            "file": ENRICHED_FILE,
            "line": ENRICHED_LINE,
            "text": ENRICHED_TEXT,
        });

        let serving = fixture_graph(&cancellation, UtcMicros(i64::MAX));
        let enriched = grep_token(project.path(), &serving).await;
        assert_eq!(
            enriched["results"],
            json!([{
                "file": ENRICHED_FILE,
                "line": ENRICHED_LINE,
                "text": ENRICHED_TEXT,
                "symbol": ENRICHED_SYMBOL,
                "node_id": ENRICHED_OCCURRENCE,
            }]),
            "the same fixture generation does enrich this hit when it answers"
        );
        assert_eq!(
            enriched["graph_enrichment"],
            json!({"status": "complete", "enriched": 1, "returned": 1})
        );

        // The generation is published, but the catalog-backed page refuses.
        let refusing = fixture_graph(&cancellation, UtcMicros(1));
        let refused = grep_token(project.path(), &refusing).await;
        assert_eq!(
            refused["results"],
            json!([lexical_hit]),
            "the lexical answer survives the refusal, and claims no symbol"
        );
        assert_eq!(refused["match_count"], 1);
        assert_eq!(
            refused["graph_enrichment"],
            json!({
                "status": "unavailable",
                "reason_code": "code-graph-timed-out",
                "retryable": true,
                "detail": "the code-graph read timed out",
            }),
            "the refusal is reported beside the hits rather than replacing them"
        );
    }
}
