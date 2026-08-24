//! Content-search tool handler: `tracedecay_grep`.
//!
//! Literal/regex search over the project working tree (respecting
//! `.gitignore`), graph-enriched: each hit resolves the enclosing symbol from
//! the code graph so the natural follow-up is `tracedecay_body`. This closes
//! the gap that made agents fall back to raw `rg` — `tracedecay_search` only
//! matches symbol *names*, not file *content*.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ignore::WalkBuilder;
use ignore::overrides::{Override, OverrideBuilder};
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{CancelSearchOnDrop, filter_by_scope, unique_file_paths};

/// Hard cap on `max_results` regardless of what the caller requests.
const MAX_RESULTS_CAP: usize = 200;
/// Default `max_results` when the caller omits it.
const DEFAULT_MAX_RESULTS: usize = 50;
/// Hard cap on `context_lines`.
const MAX_CONTEXT_LINES: usize = 3;
/// Per-file hit cap, so one noisy file cannot crowd out the rest of the tree.
const MAX_HITS_PER_FILE: usize = 20;
/// Bytes sniffed from the head of each file to classify it as binary.
const BINARY_SNIFF_BYTES: usize = 8_192;
/// Skip individual lines longer than this (minified bundles, embedded blobs).
const MAX_LINE_BYTES: usize = 4_096;
/// Skip files too large for a bounded interactive content search.
const MAX_FILE_BYTES: u64 = 2_000_000;
/// Bound each grep request, including time spent waiting for a worker permit.
const GREP_SCAN_TIMEOUT: Duration = Duration::from_secs(10);
/// Keep concurrent blocking scans from monopolizing the daemon's worker pool.
static GREP_SCAN_SEMAPHORE: Semaphore = Semaphore::const_new(2);

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

/// Handles `tracedecay_grep` tool calls.
pub(super) async fn handle_grep(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
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

    let matcher = build_matcher(pattern, fixed_strings, case_sensitive)?;

    let project_root = cg.project_root().to_path_buf();

    // Optional path filter. A caller-supplied glob whitelists candidate files
    // via the `ignore` crate's override mechanism (same glob semantics as a
    // `.gitignore` line), so it prunes at the walker level.
    let overrides = match path_glob.as_deref() {
        Some(raw) if !raw.trim().is_empty() => {
            let mut builder = OverrideBuilder::new(&project_root);
            builder.add(raw).map_err(|err| TraceDecayError::Config {
                message: format!("invalid path_glob '{raw}': {err}"),
            })?;
            Some(builder.build().map_err(|err| TraceDecayError::Config {
                message: format!("invalid path_glob '{raw}': {err}"),
            })?)
        }
        _ => None,
    };

    // Collect one extra past the cap so we can honestly report truncation.
    let scan = scan_tree_off_thread(
        project_root,
        matcher,
        overrides,
        path_glob,
        context_lines,
        max_results,
    )
    .await?;

    // Scope filtering mirrors `tracedecay_search`: when the client pins a
    // subtree, only hits under it are returned.
    let mut hits = filter_by_scope(scan.hits, scope_prefix, |hit| hit.file.as_str());
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
    let output_value = build_output_value(&hits, truncated, scan.files_scanned);

    let text = render::finalize(Some(cg.project_root()), &args, &output_value, || {
        render_grep_md(&hits, truncated, scan.files_scanned)
    });
    Ok(ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    ))
}

/// Builds the line matcher. Fixed-string search escapes the pattern so regex
/// metacharacters are treated literally; case-insensitivity is the default.
fn build_matcher(pattern: &str, fixed_strings: bool, case_sensitive: bool) -> Result<Regex> {
    let source = if fixed_strings {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    RegexBuilder::new(&source)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|err| TraceDecayError::Config {
            message: format!("invalid regex pattern '{pattern}': {err}"),
        })
}

async fn scan_tree_off_thread(
    project_root: PathBuf,
    matcher: Regex,
    overrides: Option<Override>,
    path_glob: Option<String>,
    context_lines: usize,
    max_results: usize,
) -> Result<ScanResult> {
    let query = matcher.as_str().to_string();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelSearchOnDrop::new(cancelled.clone());
    let worker_cancelled = cancelled.clone();
    let worker_query = query.clone();

    let result = tokio::time::timeout(GREP_SCAN_TIMEOUT, async move {
        let permit =
            GREP_SCAN_SEMAPHORE
                .acquire()
                .await
                .map_err(|err| TraceDecayError::Search {
                    message: format!("grep scan concurrency gate closed: {err}"),
                    query: worker_query.clone(),
                })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            scan_tree(
                &project_root,
                &matcher,
                overrides,
                path_glob.as_deref(),
                context_lines,
                max_results,
                || worker_cancelled.load(Ordering::Acquire),
            )
        })
        .await
        .map_err(|err| TraceDecayError::Search {
            message: format!("grep scan worker failed: {err}"),
            query: worker_query,
        })
    })
    .await;

    match result {
        Ok(scan) => {
            drop(cancel_on_drop);
            scan
        }
        Err(_) => Err(TraceDecayError::Search {
            message: format!(
                "grep scan timed out after {} seconds; narrow the search with path_glob",
                GREP_SCAN_TIMEOUT.as_secs()
            ),
            query,
        }),
    }
}

struct ScanResult {
    hits: Vec<GrepHit>,
    files_scanned: usize,
    truncated: bool,
}

struct GeneratedDirScope {
    literal_prefix: PathBuf,
    may_match_descendants: bool,
}

impl GeneratedDirScope {
    fn from_path_glob(path_glob: &str) -> Option<Self> {
        let path_glob = path_glob.trim();
        if path_glob.is_empty() || path_glob.starts_with('!') {
            return None;
        }
        let segments: Vec<&str> = path_glob
            .trim_start_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        let matches_basename_at_any_depth = !path_glob.contains('/');
        let wildcard_start = segments
            .iter()
            .position(|segment| {
                segment.contains('*')
                    || segment.contains('?')
                    || segment.contains('[')
                    || segment.contains('{')
            })
            .unwrap_or(segments.len());
        let literal_prefix = if matches_basename_at_any_depth {
            PathBuf::new()
        } else {
            segments[..wildcard_start]
                .iter()
                .fold(PathBuf::new(), |mut prefix, segment| {
                    prefix.push(segment);
                    prefix
                })
        };
        let wildcard_suffix = &segments[wildcard_start..];
        let may_match_descendants = matches_basename_at_any_depth
            || wildcard_suffix
                .iter()
                .enumerate()
                .any(|(index, segment)| index > 0 || *segment == "**");

        Some(Self {
            literal_prefix,
            may_match_descendants,
        })
    }

    fn allows(&self, project_root: &Path, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(project_root) else {
            return false;
        };
        if self.literal_prefix.as_os_str().is_empty() {
            return self.may_match_descendants;
        }
        self.literal_prefix.starts_with(relative)
            || relative == self.literal_prefix
            || (self.may_match_descendants && relative.starts_with(&self.literal_prefix))
    }
}

/// Walks the working tree respecting `.gitignore`, skipping binary files, and
/// collects matching lines. Stops early once `max_results` + 1 hits are found
/// so the caller can report truncation without scanning the whole tree.
fn scan_tree<F>(
    project_root: &Path,
    matcher: &Regex,
    overrides: Option<Override>,
    path_glob: Option<&str>,
    context_lines: usize,
    max_results: usize,
    is_cancelled: F,
) -> ScanResult
where
    F: Fn() -> bool,
{
    let has_positive_override = overrides
        .as_ref()
        .is_some_and(|overrides| overrides.num_whitelists() > 0);
    let generated_dir_overrides = overrides.clone();
    let generated_dir_scope = path_glob.and_then(GeneratedDirScope::from_path_glob);
    let filter_root = project_root.to_path_buf();
    let mut builder = WalkBuilder::new(project_root);
    builder
        .follow_links(false)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".gitignore")
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let segment = entry.file_name().to_string_lossy();
            if segment == ".git" || segment == ".tracedecay" {
                return false;
            }
            let requested_generated_dir = has_positive_override
                && (generated_dir_overrides
                    .as_ref()
                    .is_some_and(|overrides| overrides.matched(entry.path(), true).is_whitelist())
                    || generated_dir_scope
                        .as_ref()
                        .is_some_and(|scope| scope.allows(&filter_root, entry.path())));
            !entry.file_type().is_some_and(|kind| kind.is_dir())
                || requested_generated_dir
                || !crate::config::is_generated_dir_segment(&segment)
        });
    if let Some(overrides) = overrides {
        builder.overrides(overrides);
    }
    let walker = builder.build();

    let mut hits: Vec<GrepHit> = Vec::new();
    let mut files_scanned = 0usize;
    let mut truncated = false;

    for entry in walker {
        if is_cancelled() {
            break;
        }
        let Ok(entry) = entry else { continue };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(project_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        if is_cancelled() {
            break;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        if is_cancelled() {
            break;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if looks_binary(&bytes) {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        files_scanned += 1;

        let lines: Vec<&str> = content.lines().collect();
        let mut file_hits = 0usize;
        for (idx, line) in lines.iter().enumerate() {
            if is_cancelled() {
                return ScanResult {
                    hits,
                    files_scanned,
                    truncated,
                };
            }
            if line.len() > MAX_LINE_BYTES {
                continue;
            }
            if !matcher.is_match(line) {
                continue;
            }
            if file_hits >= MAX_HITS_PER_FILE {
                truncated = true;
                break;
            }
            file_hits += 1;

            let before = context_slice(&lines, idx.saturating_sub(context_lines), idx);
            let after = context_slice(&lines, idx + 1, (idx + 1 + context_lines).min(lines.len()));
            hits.push(GrepHit {
                file: rel_str.clone(),
                line: (idx as u32) + 1,
                text: (*line).to_string(),
                before,
                after,
                symbol_name: None,
                symbol_id: None,
                symbol_kind: None,
            });

            // Collect one past the cap so truncation is honest without paying
            // for a full-tree scan on high-frequency patterns.
            if hits.len() > max_results {
                truncated = true;
                return ScanResult {
                    hits,
                    files_scanned,
                    truncated,
                };
            }
        }
    }

    ScanResult {
        hits,
        files_scanned,
        truncated,
    }
}

fn context_slice(lines: &[&str], start: usize, end: usize) -> Vec<String> {
    if start >= end {
        return Vec::new();
    }
    lines[start..end].iter().map(|l| (*l).to_string()).collect()
}

/// Classifies a byte buffer as binary when a NUL byte appears in the head.
/// This is the same heuristic `git` and `ripgrep` use for text detection.
fn looks_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    head.contains(&0)
}

fn build_output_value(hits: &[GrepHit], truncated: bool, files_scanned: usize) -> Value {
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

    json!({
        "results": items,
        "match_count": hits.len(),
        "files_scanned": files_scanned,
        "truncated": truncated,
    })
}

fn render_grep_md(hits: &[GrepHit], truncated: bool, files_scanned: usize) -> String {
    let mut md = Md::new();
    md.heading(2, "Grep Results");
    if hits.is_empty() {
        md.empty_note("No matching lines.");
        md.line(&format!("_Scanned {files_scanned} files._"));
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
    md.render()
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let matcher = Regex::new("UNIQUE_GENERATED_DIR_TOKEN").expect("matcher");
        let scan = scan_tree(project.path(), &matcher, None, None, 0, 10, || false);
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

        let overrides = |root: &Path| {
            let mut builder = OverrideBuilder::new(root);
            builder.add("src/**").expect("path glob");
            builder.build().expect("overrides")
        };
        let matcher = Regex::new("NORMAL_PATH_GLOB_TOKEN").expect("matcher");
        let baseline_checks = std::sync::atomic::AtomicUsize::new(0);
        let baseline = scan_tree(
            project.path(),
            &matcher,
            Some(overrides(project.path())),
            Some("src/**"),
            0,
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
        let scan = scan_tree(
            project.path(),
            &matcher,
            Some(overrides(project.path())),
            Some("src/**"),
            0,
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

        let mut builder = OverrideBuilder::new(project.path());
        builder.add("*.js").expect("path glob");
        let overrides = builder.build().expect("overrides");
        let matcher = Regex::new("SLASHLESS_GENERATED_GLOB_TOKEN").expect("matcher");
        let scan = scan_tree(
            project.path(),
            &matcher,
            Some(overrides),
            Some("*.js"),
            0,
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

    #[test]
    fn scan_tree_stops_when_cancelled_during_line_matching() {
        let project = tempfile::tempdir().expect("temp project");
        let source = "CANCELLATION_TOKEN\n".repeat(100);
        std::fs::write(project.path().join("fixture.txt"), source).expect("fixture");

        let matcher = Regex::new("CANCELLATION_TOKEN").expect("matcher");
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let scan = scan_tree(project.path(), &matcher, None, None, 0, 200, || {
            checks.fetch_add(1, Ordering::Relaxed) >= 10
        });

        assert!(
            scan.hits.len() < MAX_HITS_PER_FILE,
            "cancelled scan should stop before the per-file cap: {}",
            scan.hits.len()
        );
        assert!(checks.load(Ordering::Relaxed) > 10);
    }

    #[test]
    fn scan_tree_skips_files_larger_than_two_megabytes() {
        let project = tempfile::tempdir().expect("temp project");
        let mut oversized = b"OVERSIZED_FILE_TOKEN\n".to_vec();
        oversized.resize((MAX_FILE_BYTES as usize) + 1, b'x');
        std::fs::write(project.path().join("oversized.txt"), oversized).expect("oversized fixture");
        std::fs::write(project.path().join("tracked.txt"), "OVERSIZED_FILE_TOKEN\n")
            .expect("source fixture");

        let matcher = Regex::new("OVERSIZED_FILE_TOKEN").expect("matcher");
        let scan = scan_tree(project.path(), &matcher, None, None, 0, 10, || false);
        let files = scan
            .hits
            .iter()
            .map(|hit| hit.file.as_str())
            .collect::<Vec<_>>();

        assert!(files.contains(&"tracked.txt"), "{files:?}");
        assert!(!files.contains(&"oversized.txt"), "{files:?}");
    }
}
