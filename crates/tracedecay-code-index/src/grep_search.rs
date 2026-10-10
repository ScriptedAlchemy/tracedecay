use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use ignore::DirEntry;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use regex::{Regex, RegexBuilder};
use tracedecay_domain::IndexPathPolicyV1;

use crate::parallelism::{CodeIndexParallelismErrorV1, install, with_background_cpu_permit};
use crate::source_walk::{forward_slash_relative, source_walk};

const MAX_HITS_PER_FILE: usize = 20;
const BINARY_SNIFF_BYTES: usize = 8_192;
pub const MAX_LINE_BYTES: usize = 4_096;
pub const MAX_INTERACTIVE_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct GrepSearchQuery {
    pub pattern: String,
    pub fixed_strings: bool,
    pub case_sensitive: bool,
    pub path_glob: Option<String>,
    pub context_lines: usize,
    pub max_results: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepSearchHit {
    pub file: Arc<str>,
    pub line: u32,
    pub text: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrepSearchResult {
    pub hits: Vec<GrepSearchHit>,
    pub files_scanned: usize,
    pub lines_examined: usize,
    /// Line slices pulled from source text. Early match, cancel, and
    /// per-file caps must stop pulling; materializing every line first
    /// makes this equal the file's line count even when examination stops.
    pub lines_visited: usize,
    pub omissions: GrepScanOmissionsV1,
    pub truncated: bool,
    pub cancelled: bool,
}

/// Sources the bounded scan deliberately skipped, so callers can report
/// partial coverage instead of implying a complete answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GrepScanOmissionsV1 {
    pub oversized_files: usize,
    pub oversized_lines: usize,
    pub unavailable_sources: usize,
}

impl GrepScanOmissionsV1 {
    #[must_use]
    pub fn any(self) -> bool {
        self.oversized_files > 0 || self.oversized_lines > 0 || self.unavailable_sources > 0
    }

    /// Omissions caused by the scan's own byte budgets (as opposed to sources
    /// that could not be read at all).
    #[must_use]
    pub fn budget(self) -> usize {
        self.oversized_files + self.oversized_lines
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrepSearchError {
    InvalidPattern { pattern: String, message: String },
    InvalidGlob { glob: String, message: String },
    Parallelism(CodeIndexParallelismErrorV1),
}

impl std::fmt::Display for GrepSearchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPattern { pattern, message } => {
                write!(formatter, "invalid regex pattern '{pattern}': {message}")
            }
            Self::InvalidGlob { glob, message } => {
                write!(formatter, "invalid path_glob '{glob}': {message}")
            }
            Self::Parallelism(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for GrepSearchError {}

#[tracing::instrument(name = "code_index.search.grep", level = "trace", skip_all)]
pub fn search_tree_with_cancel(
    project_root: &Path,
    query: &GrepSearchQuery,
    path_policy: &IndexPathPolicyV1,
    is_cancelled: impl Fn() -> bool + Sync,
) -> Result<GrepSearchResult, GrepSearchError> {
    let matcher = build_matcher(query)?;
    let mut walker =
        source_walk(project_root, query.path_glob.as_deref(), path_policy).map_err(|error| {
            GrepSearchError::InvalidGlob {
                glob: error.glob,
                message: error.message,
            }
        })?;
    install(|| {
        let mut result = GrepSearchResult::default();
        let max_results = query.max_results.max(1);
        // Bound retained source and reads to the existing owner's worker width.
        // Indexed collection preserves walk order before applying the hit cap.
        let width = rayon::current_num_threads();
        loop {
            let mut batch = Vec::with_capacity(width);
            while batch.len() < width {
                if is_cancelled() {
                    result.cancelled = true;
                    return result;
                }
                let Some(entry) = walker.next() else { break };
                match entry {
                    Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => {
                        batch.push(entry);
                    }
                    Ok(_) => {}
                    Err(_) => result.omissions.unavailable_sources += 1,
                }
            }
            if batch.is_empty() {
                return result;
            }
            let scans = batch
                .par_iter()
                .map(|entry| {
                    with_background_cpu_permit(|| {
                        scan_grep_file(entry, project_root, &matcher, query, &is_cancelled)
                    })
                })
                .collect::<Vec<_>>();
            for scan in scans {
                result.files_scanned += scan.files_scanned;
                result.lines_examined += scan.lines_examined;
                result.lines_visited += scan.lines_visited;
                result.omissions.oversized_files += scan.omissions.oversized_files;
                result.omissions.oversized_lines += scan.omissions.oversized_lines;
                result.omissions.unavailable_sources += scan.omissions.unavailable_sources;
                result.truncated |= scan.truncated;
                result.cancelled |= scan.cancelled;
                let remaining = max_results
                    .saturating_add(1)
                    .saturating_sub(result.hits.len());
                if scan.hits.len() > remaining {
                    result.truncated = true;
                }
                result.hits.extend(scan.hits.into_iter().take(remaining));
            }
            if result.cancelled || result.hits.len() > max_results {
                result.truncated |= result.hits.len() > max_results;
                return result;
            }
        }
    })
    .map_err(GrepSearchError::Parallelism)
}

fn scan_grep_file(
    entry: &DirEntry,
    project_root: &Path,
    matcher: &Regex,
    query: &GrepSearchQuery,
    is_cancelled: &(impl Fn() -> bool + Sync),
) -> GrepSearchResult {
    let mut result = GrepSearchResult::default();
    let max_results = query.max_results.max(1);
    if is_cancelled() {
        result.cancelled = true;
        return result;
    }
    let path = entry.path();
    let Ok(relative) = path.strip_prefix(project_root) else {
        return result;
    };
    let Ok(metadata) = entry.metadata() else {
        result.omissions.unavailable_sources += 1;
        return result;
    };
    if metadata.len() > MAX_INTERACTIVE_SOURCE_BYTES {
        result.omissions.oversized_files += 1;
        return result;
    }
    if is_cancelled() {
        result.cancelled = true;
        return result;
    }
    let Ok(bytes) = std::fs::read(path) else {
        result.omissions.unavailable_sources += 1;
        return result;
    };
    if looks_binary(&bytes) {
        return result;
    }
    let Ok(content) = String::from_utf8(bytes) else {
        result.omissions.unavailable_sources += 1;
        return result;
    };

    // Defer path materialization until the file yields a hit so zero-hit
    // files never pay normalize+alloc on the grep hot path.
    if crate::observe::sample_hot_loop() {
        {
            let _span = tracing::trace_span!("code_index_grep_file").entered();
            examine_grep_file(
                matcher,
                query,
                relative,
                &content,
                &mut result,
                max_results,
                is_cancelled,
            )
        }
    } else {
        examine_grep_file(
            matcher,
            query,
            relative,
            &content,
            &mut result,
            max_results,
            is_cancelled,
        )
    };
    result
}

fn examine_grep_file<C: Fn() -> bool>(
    matcher: &Regex,
    query: &GrepSearchQuery,
    relative: &Path,
    content: &str,
    result: &mut GrepSearchResult,
    max_results: usize,
    is_cancelled: &C,
) -> bool {
    result.files_scanned += 1;
    let context_lines = query.context_lines;
    let mut before = VecDeque::new();
    let mut pending = VecDeque::new();
    let mut source = content.lines().enumerate();
    let mut file_hits = 0;
    let mut relative_key: Option<Arc<str>> = None;
    while let Some((index, line)) = next_grep_line(&mut source, &mut pending, result) {
        if is_cancelled() {
            result.cancelled = true;
            return true;
        }
        if line.len() > MAX_LINE_BYTES {
            result.omissions.oversized_lines += 1;
            remember_before(&mut before, line, context_lines);
            continue;
        }
        result.lines_examined += 1;
        if !matcher.is_match(line) {
            remember_before(&mut before, line, context_lines);
            continue;
        }
        if file_hits >= MAX_HITS_PER_FILE {
            result.truncated = true;
            break;
        }
        file_hits += 1;
        fill_after_context(&mut source, &mut pending, result, context_lines);
        let file = Arc::clone(relative_key.get_or_insert_with(|| forward_slash_relative(relative)));
        result.hits.push(GrepSearchHit {
            file,
            line: index as u32 + 1,
            text: line.to_owned(),
            before: before.iter().copied().map(str::to_owned).collect(),
            after: pending
                .iter()
                .take(context_lines)
                .map(|(_, peeked)| (*peeked).to_owned())
                .collect(),
        });
        remember_before(&mut before, line, context_lines);
        // Collect one past the cap so callers can report truncation
        // without scanning the remainder of a high-frequency tree.
        if result.hits.len() > max_results {
            result.truncated = true;
            return true;
        }
    }
    false
}

fn next_grep_line<'a>(
    source: &mut impl Iterator<Item = (usize, &'a str)>,
    pending: &mut VecDeque<(usize, &'a str)>,
    result: &mut GrepSearchResult,
) -> Option<(usize, &'a str)> {
    if let Some(line) = pending.pop_front() {
        return Some(line);
    }
    let line = source.next()?;
    result.lines_visited = result.lines_visited.saturating_add(1);
    Some(line)
}

fn fill_after_context<'a>(
    source: &mut impl Iterator<Item = (usize, &'a str)>,
    pending: &mut VecDeque<(usize, &'a str)>,
    result: &mut GrepSearchResult,
    context_lines: usize,
) {
    while pending.len() < context_lines {
        let Some(line) = source.next() else {
            break;
        };
        result.lines_visited = result.lines_visited.saturating_add(1);
        pending.push_back(line);
    }
}

fn remember_before<'a>(before: &mut VecDeque<&'a str>, line: &'a str, context_lines: usize) {
    if context_lines == 0 {
        return;
    }
    if before.len() == context_lines {
        before.pop_front();
    }
    before.push_back(line);
}

fn build_matcher(query: &GrepSearchQuery) -> Result<Regex, GrepSearchError> {
    let source = if query.fixed_strings {
        regex::escape(&query.pattern)
    } else {
        query.pattern.clone()
    };
    RegexBuilder::new(&source)
        .case_insensitive(!query.case_sensitive)
        .build()
        .map_err(|error| GrepSearchError::InvalidPattern {
            pattern: query.pattern.clone(),
            message: error.to_string(),
        })
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_exclusions() -> IndexPathPolicyV1 {
        IndexPathPolicyV1::new(Vec::new(), Vec::new()).unwrap()
    }

    fn query(pattern: &str) -> GrepSearchQuery {
        GrepSearchQuery {
            pattern: pattern.to_owned(),
            fixed_strings: false,
            case_sensitive: true,
            path_glob: None,
            context_lines: 0,
            max_results: 10,
        }
    }

    #[test]
    fn parallel_reads_preserve_walk_order_and_the_global_cap() {
        let project = tempfile::tempdir().unwrap();
        for index in 0..8 {
            std::fs::write(project.path().join(format!("{index}.rs")), "HIT_TOKEN\n").unwrap();
        }
        let mut query = query("HIT_TOKEN");
        query.max_results = 1;
        let scan = |workers| {
            let runtime = crate::parallelism::CodeIndexWorkerRuntimeV1::build(
                tracedecay_domain::configuration::CodeIndexWorkerSelectionV1::Exact { workers },
                4,
                16 * 1024 * 1024 * 1024,
            )
            .unwrap();
            let _entered = runtime.enter();
            search_tree_with_cancel(project.path(), &query, &no_exclusions(), || false).unwrap()
        };
        let serial = scan(1);
        let parallel = scan(4);
        assert_eq!(serial.hits, parallel.hits);
        assert_eq!(parallel.hits.len(), 2);
        assert!(parallel.truncated);
        assert!(!parallel.cancelled);
        assert!(parallel.files_scanned <= 4);
    }

    #[test]
    fn worker_pool_failure_is_typed_instead_of_an_empty_scan() {
        let project = tempfile::tempdir().unwrap();
        crate::parallelism::force_install_failure_for_test(true);
        let outcome =
            search_tree_with_cancel(project.path(), &query("token"), &no_exclusions(), || false);
        crate::parallelism::force_install_failure_for_test(false);
        assert!(matches!(outcome, Err(GrepSearchError::Parallelism(_))));
    }

    #[test]
    fn cancellation_stops_during_line_matching() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("fixture.txt"),
            "CANCEL_TOKEN\n".repeat(100),
        )
        .unwrap();
        let checks = std::sync::atomic::AtomicUsize::new(0);

        let result = search_tree_with_cancel(
            project.path(),
            &query("CANCEL_TOKEN"),
            &no_exclusions(),
            || checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 10,
        )
        .unwrap();

        assert!(result.cancelled);
        assert!(result.hits.len() < MAX_HITS_PER_FILE);
    }

    #[test]
    fn files_above_two_mibibytes_are_not_read() {
        let project = tempfile::tempdir().unwrap();
        let mut oversized = b"FILE_CAP_TOKEN\n".to_vec();
        oversized.resize(MAX_INTERACTIVE_SOURCE_BYTES as usize + 1, b'x');
        std::fs::write(project.path().join("oversized.txt"), oversized).unwrap();
        std::fs::write(project.path().join("tracked.txt"), "FILE_CAP_TOKEN\n").unwrap();

        let result = search_tree_with_cancel(
            project.path(),
            &query("FILE_CAP_TOKEN"),
            &no_exclusions(),
            || false,
        )
        .unwrap();

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].file.as_ref(), "tracked.txt");
    }

    #[test]
    fn early_file_cap_does_not_visit_unexamined_lines() {
        let project = tempfile::tempdir().unwrap();
        const TOTAL_LINES: usize = 8_192;
        std::fs::write(
            project.path().join("dense.txt"),
            "HIT_TOKEN\n".repeat(TOTAL_LINES),
        )
        .unwrap();
        let mut query = query("HIT_TOKEN");
        query.max_results = 1;

        let result =
            search_tree_with_cancel(project.path(), &query, &no_exclusions(), || false).unwrap();

        assert_eq!(result.hits.len(), 2);
        assert!(result.truncated);
        assert_eq!(result.lines_examined, 2);
        assert!(
            result.lines_visited <= result.lines_examined + query.context_lines,
            "visited {} lines after examining {}; full collect materializes every line",
            result.lines_visited,
            result.lines_examined
        );
        assert!(result.lines_visited < TOTAL_LINES);
    }
}
