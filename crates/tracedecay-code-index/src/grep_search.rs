use std::collections::VecDeque;
use std::path::Path;

use regex::{Regex, RegexBuilder};

use crate::source_walk::source_walk;

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
    pub file: String,
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
        }
    }
}

impl std::error::Error for GrepSearchError {}

#[hotpath::measure(label = "code_index.search.grep")]
pub fn search_tree_with_cancel(
    project_root: &Path,
    query: &GrepSearchQuery,
    is_cancelled: impl Fn() -> bool,
) -> Result<GrepSearchResult, GrepSearchError> {
    let matcher = build_matcher(query)?;
    let walker = source_walk(project_root, query.path_glob.as_deref()).map_err(|error| {
        GrepSearchError::InvalidGlob {
            glob: error.glob,
            message: error.message,
        }
    })?;
    let mut result = GrepSearchResult::default();
    let max_results = query.max_results.max(1);
    #[cfg(feature = "hotpath")]
    let mut source_bytes = 0_u64;

    for entry in walker {
        if is_cancelled() {
            result.cancelled = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(project_root) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            result.omissions.unavailable_sources += 1;
            continue;
        };
        if metadata.len() > MAX_INTERACTIVE_SOURCE_BYTES {
            result.omissions.oversized_files += 1;
            continue;
        }
        if is_cancelled() {
            result.cancelled = true;
            break;
        }
        let Ok(bytes) = std::fs::read(path) else {
            result.omissions.unavailable_sources += 1;
            continue;
        };
        if looks_binary(&bytes) {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            result.omissions.unavailable_sources += 1;
            continue;
        };
        #[cfg(feature = "hotpath")]
        {
            source_bytes = source_bytes.saturating_add(content.len() as u64);
        }
        let relative = relative.to_string_lossy().replace('\\', "/");
        let stop = if crate::hotpath_observe::sample_hot_loop() {
            hotpath::measure_block!(
                "code_index_grep_file",
                examine_grep_file(
                    &matcher,
                    query,
                    &relative,
                    &content,
                    &mut result,
                    max_results,
                    &is_cancelled,
                )
            )
        } else {
            examine_grep_file(
                &matcher,
                query,
                &relative,
                &content,
                &mut result,
                max_results,
                &is_cancelled,
            )
        };
        if stop {
            crate::hotpath_observe::record_files(result.files_scanned);
            #[cfg(feature = "hotpath")]
            crate::hotpath_observe::record_source_bytes(source_bytes);
            return Ok(result);
        }
    }
    crate::hotpath_observe::record_files(result.files_scanned);
    #[cfg(feature = "hotpath")]
    crate::hotpath_observe::record_source_bytes(source_bytes);
    Ok(result)
}

fn examine_grep_file<C: Fn() -> bool>(
    matcher: &Regex,
    query: &GrepSearchQuery,
    relative: &str,
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
        result.hits.push(GrepSearchHit {
            file: relative.to_owned(),
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
    fn cancellation_stops_during_line_matching() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("fixture.txt"),
            "CANCEL_TOKEN\n".repeat(100),
        )
        .unwrap();
        let checks = std::sync::atomic::AtomicUsize::new(0);

        let result = search_tree_with_cancel(project.path(), &query("CANCEL_TOKEN"), || {
            checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 10
        })
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

        let result =
            search_tree_with_cancel(project.path(), &query("FILE_CAP_TOKEN"), || false).unwrap();

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].file, "tracked.txt");
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

        let result = search_tree_with_cancel(project.path(), &query, || false).unwrap();

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

    #[test]
    fn context_windows_include_oversized_neighbors_without_full_collect() {
        let project = tempfile::tempdir().unwrap();
        let oversized = "X".repeat(MAX_LINE_BYTES + 1);
        let body = format!("before\n{oversized}\nHIT_TOKEN\nafter\nTAIL\n");
        std::fs::write(project.path().join("ctx.txt"), body).unwrap();
        let mut query = query("HIT_TOKEN");
        query.context_lines = 1;
        query.max_results = 1;

        let result = search_tree_with_cancel(project.path(), &query, || false).unwrap();

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].line, 3);
        assert_eq!(result.hits[0].before, vec![oversized]);
        assert_eq!(result.hits[0].after, vec!["after".to_owned()]);
        assert_eq!(result.omissions.oversized_lines, 1);
        assert_eq!(result.lines_examined, 4);
        assert!(
            result.lines_visited <= result.lines_examined + result.omissions.oversized_lines,
            "visited {} after examining {} plus one oversized neighbor",
            result.lines_visited,
            result.lines_examined
        );
    }

    #[test]
    fn path_glob_does_not_visit_out_of_scope_files() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("keep.txt"), "SCOPE_TOKEN\nextra\n").unwrap();
        std::fs::write(
            project.path().join("skip.txt"),
            "SCOPE_TOKEN\n".repeat(4_096),
        )
        .unwrap();
        let mut query = query("SCOPE_TOKEN");
        query.path_glob = Some("keep.txt".to_owned());
        query.max_results = 1;

        let result = search_tree_with_cancel(project.path(), &query, || false).unwrap();

        assert_eq!(result.files_scanned, 1);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].file, "keep.txt");
        assert!(result.lines_visited < 4_096);
    }
}
