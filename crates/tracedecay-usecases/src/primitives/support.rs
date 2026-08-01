use std::collections::{HashMap, HashSet};
use std::path::Path;

use ignore::WalkBuilder;
use ignore::overrides::Override;
use regex::{Regex, RegexBuilder};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

use crate::tracedecay::TraceDecay;

pub(crate) struct GrepHit {
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) text: String,
    pub(crate) before: Vec<String>,
    pub(crate) after: Vec<String>,
}

pub(crate) fn build_matcher(
    pattern: &str,
    fixed_strings: bool,
    case_sensitive: bool,
) -> Result<Regex> {
    let source = if fixed_strings {
        regex::escape(pattern)
    } else {
        pattern.to_owned()
    };
    RegexBuilder::new(&source)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid regex pattern '{pattern}': {error}"),
        })
}

pub(crate) struct ScanResult {
    pub(crate) hits: Vec<GrepHit>,
    pub(crate) files_scanned: usize,
    pub(crate) truncated: bool,
}

pub(crate) fn scan_tree(
    project_root: &Path,
    matcher: &Regex,
    overrides: Option<Override>,
    context_lines: usize,
    max_results: usize,
) -> ScanResult {
    let mut builder = WalkBuilder::new(project_root);
    builder
        .follow_links(false)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".gitignore");
    if let Some(overrides) = overrides {
        builder.overrides(overrides);
    }
    let mut hits = Vec::new();
    let mut files_scanned = 0;
    let mut truncated = false;
    for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(project_root) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes[..bytes.len().min(8_192)].contains(&0) {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        files_scanned += 1;
        let lines = content.lines().collect::<Vec<_>>();
        let mut file_hits = 0;
        for (index, line) in lines.iter().enumerate() {
            if line.len() > 4_096 || !matcher.is_match(line) {
                continue;
            }
            if file_hits >= 20 {
                truncated = true;
                break;
            }
            file_hits += 1;
            hits.push(GrepHit {
                file: relative.to_string_lossy().replace('\\', "/"),
                line: index as u32 + 1,
                text: (*line).to_owned(),
                before: context_slice(&lines, index.saturating_sub(context_lines), index),
                after: context_slice(
                    &lines,
                    index + 1,
                    (index + 1 + context_lines).min(lines.len()),
                ),
            });
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
    lines
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RankedAffectedTest {
    pub(crate) path: String,
    pub(crate) distance: usize,
}

pub(crate) struct AffectedTestTraversal {
    pub(crate) test_distances: HashMap<String, usize>,
}

pub(crate) fn rank_affected_tests(
    test_distances: &HashMap<String, usize>,
) -> Vec<RankedAffectedTest> {
    let mut ranked = test_distances
        .iter()
        .map(|(path, distance)| RankedAffectedTest {
            path: path.clone(),
            distance: *distance,
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked
}

pub(crate) const fn affected_test_proximity(distance: usize) -> &'static str {
    match distance {
        0 => "changed",
        1 => "direct",
        2 => "near",
        _ => "transitive",
    }
}

pub(crate) async fn collect_affected_test_files(
    graph: &TraceDecay,
    files: &[String],
    max_depth: usize,
    custom_glob: Option<&glob::Pattern>,
    files_with_inline_tests: &HashSet<String>,
) -> Result<AffectedTestTraversal> {
    let is_test = |path: &str| {
        custom_glob.map_or_else(
            || tracedecay_code_index::is_test_file(path) || files_with_inline_tests.contains(path),
            |pattern| pattern.matches(path),
        )
    };
    let mut test_distances = HashMap::new();
    let mut visited = HashSet::new();
    let mut frontier = Vec::new();
    for file in files {
        if is_test(file) {
            test_distances.insert(file.clone(), 0);
        }
        if visited.insert(file.clone()) {
            frontier.push(file.clone());
        }
    }
    frontier.sort();
    for depth in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        let mut dependents = Vec::new();
        for file in &frontier {
            dependents.extend(graph.get_file_dependents(file).await?);
        }
        dependents.sort();
        dependents.dedup();
        let mut next = Vec::new();
        for dependent in dependents {
            if !visited.insert(dependent.clone()) {
                continue;
            }
            if is_test(&dependent) {
                test_distances.insert(dependent, depth + 1);
            } else {
                next.push(dependent);
            }
        }
        frontier = next;
    }
    Ok(AffectedTestTraversal { test_distances })
}
