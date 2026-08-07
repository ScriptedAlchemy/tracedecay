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
        result.files_scanned += 1;
        let lines = content.lines().collect::<Vec<_>>();
        let mut file_hits = 0;
        for (index, line) in lines.iter().enumerate() {
            if is_cancelled() {
                result.cancelled = true;
                return Ok(result);
            }
            if line.len() > MAX_LINE_BYTES {
                result.omissions.oversized_lines += 1;
                continue;
            }
            result.lines_examined += 1;
            if !matcher.is_match(line) {
                continue;
            }
            if file_hits >= MAX_HITS_PER_FILE {
                result.truncated = true;
                break;
            }
            file_hits += 1;
            result.hits.push(GrepSearchHit {
                file: relative.to_string_lossy().replace('\\', "/"),
                line: index as u32 + 1,
                text: (*line).to_owned(),
                before: context_slice(&lines, index.saturating_sub(query.context_lines), index),
                after: context_slice(
                    &lines,
                    index + 1,
                    (index + 1 + query.context_lines).min(lines.len()),
                ),
            });
            // Collect one past the cap so callers can report truncation
            // without scanning the remainder of a high-frequency tree.
            if result.hits.len() > max_results {
                result.truncated = true;
                return Ok(result);
            }
        }
    }
    Ok(result)
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

fn context_slice(lines: &[&str], start: usize, end: usize) -> Vec<String> {
    lines
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
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
}
