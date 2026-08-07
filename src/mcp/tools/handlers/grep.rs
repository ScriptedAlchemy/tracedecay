//! Content-search tool handler: `tracedecay_grep`.
//!
//! Literal/regex search over UTF-8 text sources in the project working tree
//! (respecting `.gitignore`), graph-enriched: each hit resolves the enclosing
//! symbol from the code graph so the natural follow-up is `tracedecay_body`.
//! This closes the gap that made agents fall back to raw `rg` —
//! `tracedecay_search` only matches symbol *names*, not file *content*.

use std::fmt::Write as _;

use serde_json::{Value, json};
use tracedecay_application::{
    CoverageCompleteness, CoverageDomainState, EvidenceCoverage, EvidenceDomain, Omission,
    OmissionReason,
};
use tracedecay_code_index::grep_search::{
    GrepScanOmissionsV1, GrepSearchHit, GrepSearchQuery, MAX_INTERACTIVE_SOURCE_BYTES,
    MAX_LINE_BYTES, search_tree_with_cancel,
};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{filter_by_scope, run_bounded_search, unique_file_paths};

/// Hard cap on `max_results` regardless of what the caller requests.
const MAX_RESULTS_CAP: usize = 200;
/// Default `max_results` when the caller omits it.
const DEFAULT_MAX_RESULTS: usize = 50;
/// Hard cap on `context_lines`.
const MAX_CONTEXT_LINES: usize = 3;
/// A single content-search hit, enriched with the enclosing graph symbol.
struct GrepHit {
    file: String,
    line: u32,
    text: String,
    before: Vec<String>,
    after: Vec<String>,
    symbol_name: Option<String>,
    symbol_id: Option<String>,
    symbol_kind: Option<String>,
}

impl From<GrepSearchHit> for GrepHit {
    fn from(hit: GrepSearchHit) -> Self {
        Self {
            file: hit.file,
            line: hit.line,
            text: hit.text,
            before: hit.before,
            after: hit.after,
            symbol_name: None,
            symbol_id: None,
            symbol_kind: None,
        }
    }
}

/// Handles `tracedecay_grep` tool calls.
pub(super) async fn handle_grep(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
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

    let project_root = cg.project_root().to_path_buf();
    let query = GrepSearchQuery {
        pattern: pattern.to_owned(),
        fixed_strings,
        case_sensitive,
        path_glob,
        context_lines,
        max_results,
    };
    let scan = run_bounded_search(
        "tracedecay_grep",
        pattern.to_owned(),
        deadline,
        cancellation,
        move |cancelled, transport_cancellation| {
            search_tree_with_cancel(&project_root, &query, || {
                cancelled.load(std::sync::atomic::Ordering::Acquire)
                    || transport_cancellation
                        .as_ref()
                        .is_some_and(|signal| signal.is_cancelled())
            })
        },
    )
    .await?;

    // Scope filtering mirrors `tracedecay_search`: when the client pins a
    // subtree, only hits under it are returned.
    let mut hits = filter_by_scope(
        scan.hits.into_iter().map(GrepHit::from).collect(),
        scope_prefix,
        |hit| hit.file.as_str(),
    );
    let truncated = scan.truncated || hits.len() > max_results;
    hits.truncate(max_results);

    // Enrich each hit with the smallest graph node that contains it.
    for hit in &mut hits {
        if let Ok(Some(node)) = cg.node_at_location(&hit.file, hit.line).await {
            hit.symbol_name = Some(node.name);
            hit.symbol_id = Some(node.id);
            hit.symbol_kind = Some(node.kind.as_str().to_string());
        }
    }

    let touched_files = unique_file_paths(hits.iter().map(|hit| hit.file.as_str()));
    let output_value = build_output_value(
        &hits,
        truncated,
        scan.files_scanned,
        scan.lines_examined,
        scan.omissions,
    );

    let text = render::finalize(Some(cg.project_root()), &args, &output_value, || {
        render_grep_md(&hits, truncated, scan.files_scanned, scan.omissions)
    });
    Ok(ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    ))
}

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
            if let Some(name) = &hit.symbol_name {
                item["symbol"] = json!(name);
            }
            if let Some(id) = &hit.symbol_id {
                item["node_id"] = json!(id);
            }
            if let Some(kind) = &hit.symbol_kind {
                item["kind"] = json!(kind);
            }
            if !hit.before.is_empty() {
                item["before"] = json!(hit.before);
            }
            if !hit.after.is_empty() {
                item["after"] = json!(hit.after);
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
        let location = match (&hit.symbol_name, &hit.symbol_kind) {
            (Some(name), Some(kind)) => {
                format!("{}:{} — **{name}** ({kind})", hit.file, hit.line)
            }
            _ => format!("{}:{}", hit.file, hit.line),
        };
        md.bullet(&location);
        for line in &hit.before {
            md.line(&format!("    {line}"));
        }
        md.line(&format!("  > {}", hit.text));
        for line in &hit.after {
            md.line(&format!("    {line}"));
        }
        if let Some(id) = &hit.symbol_id {
            md.line(&format!(
                "  `{id}` · call `tracedecay_body` to read the symbol"
            ));
        }
    }

    md.blank();
    let mut summary = format!("_{} matches across {files_scanned} files._", hits.len());
    if truncated {
        let _ = write!(
            summary,
            " Results capped — narrow with `path_glob` or a more specific pattern."
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
            .map(|hit| hit.file.as_str())
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
            .map(|hit| hit.file.as_str())
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

    fn assert_partial_output(output: &Value, visited: u64, returned: u64, omissions: Value) {
        assert_eq!(output["truncated"], json!(false));
        assert_eq!(output["coverage"]["completeness"], json!("partial"));
        assert_eq!(output["coverage"]["visited"], json!(visited));
        assert_eq!(output["coverage"]["eligible"], Value::Null);
        assert_eq!(output["coverage"]["returned"], json!(returned));
        assert_eq!(output["omissions"], omissions);
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
        assert_partial_output(&output, 0, 0, one_budget_omission());

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
        assert_eq!(scan.hits[0].file, "tracked.txt");
        assert_eq!(scan.files_scanned, 1);
        assert_eq!(scan.lines_examined, 3);
        assert_eq!(scan.omissions.oversized_files, 1);
        assert!(!scan.truncated);
        assert_eq!(output["results"][0]["file"], json!("tracked.txt"));
        assert_partial_output(&output, 3, 1, one_budget_omission());

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
        assert_partial_output(&output, 1, 0, one_budget_omission());

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
            json!([
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
}
