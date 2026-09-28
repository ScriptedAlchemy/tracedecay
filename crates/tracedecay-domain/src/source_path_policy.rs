use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::repository_path_matches_scope;

/// Directory-name segments treated as generated or vendored content across
/// indexing, migration inventory, and interactive source traversal.
pub const GENERATED_DIR_SEGMENTS: &[&str] = &[
    ".cache",
    ".gradle",
    ".next",
    ".turbo",
    ".venv",
    ".worktrees",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "out",
    "target",
    "vendor",
    "venv",
];

#[must_use]
pub fn is_generated_dir_segment(segment: &str) -> bool {
    GENERATED_DIR_SEGMENTS.contains(&segment)
}

/// A pattern `index.exclude.v1` or `index.include.v1` cannot compile.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("index path pattern '{pattern}' is invalid: {message}")]
pub struct IndexPathPatternError {
    pub pattern: String,
    pub message: String,
}

/// The project's `index.exclude.v1` / `index.include.v1` patterns over
/// forward-slash, project-relative paths.
///
/// A pattern is a glob whose `*` and `?` stay inside one path segment and
/// whose `**` spans segments. It matches a path or any of its parent
/// directories, so `docs` and `docs/**` both cover everything under `docs/`.
/// A path is excluded when an exclude pattern matches it and no include
/// pattern does: include re-admits paths the exclude list would drop. Both
/// lists operate inside Git's view of the worktree (tracked files and
/// untracked files `.gitignore` does not ignore); neither reaches ignored
/// files.
#[derive(Clone, Debug)]
pub struct IndexPathPolicyV1 {
    exclude: Vec<String>,
    include: Vec<String>,
    exclude_set: GlobSet,
    /// Stems of `P/**` exclude patterns: a directory one matches has every
    /// descendant excluded, so a walk may skip it before listing it.
    excluded_dir_stems: GlobSet,
    include_set: GlobSet,
    /// Literal leading segments of each include pattern; `None` when the
    /// pattern starts with a wildcard and may match anywhere.
    include_prefixes: Vec<Option<String>>,
}

impl PartialEq for IndexPathPolicyV1 {
    fn eq(&self, other: &Self) -> bool {
        self.exclude == other.exclude && self.include == other.include
    }
}

impl Eq for IndexPathPolicyV1 {}

impl IndexPathPolicyV1 {
    pub fn new(exclude: Vec<String>, include: Vec<String>) -> Result<Self, IndexPathPatternError> {
        let exclude_set = compile(&exclude)?;
        let excluded_dir_stems = compile(
            &exclude
                .iter()
                .filter_map(|pattern| normalize(pattern).strip_suffix("/**"))
                .filter(|stem| !stem.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )?;
        let include_set = compile(&include)?;
        let include_prefixes = include
            .iter()
            .map(|pattern| literal_prefix(normalize(pattern)))
            .collect();
        Ok(Self {
            exclude,
            include,
            exclude_set,
            excluded_dir_stems,
            include_set,
            include_prefixes,
        })
    }

    pub fn exclude_patterns(&self) -> &[String] {
        &self.exclude
    }

    pub fn include_patterns(&self) -> &[String] {
        &self.include
    }

    /// Whether `logical_path` is left out of the index and source walks.
    #[must_use]
    pub fn excludes(&self, logical_path: &str) -> bool {
        matches_path_or_parent(&self.exclude_set, logical_path)
            && !matches_path_or_parent(&self.include_set, logical_path)
    }

    /// Whether every path under directory `logical_dir` is excluded, so a
    /// walk may skip the directory without listing it.
    #[must_use]
    pub fn excludes_directory(&self, logical_dir: &str) -> bool {
        (matches_path_or_parent(&self.exclude_set, logical_dir)
            || matches_path_or_parent(&self.excluded_dir_stems, logical_dir))
            && self.include_prefixes.iter().all(|prefix| {
                prefix.as_deref().is_some_and(|prefix| {
                    !repository_path_matches_scope(prefix, Some(logical_dir))
                        && !repository_path_matches_scope(logical_dir, Some(prefix))
                })
            })
    }
}

/// Validate one pattern list with the same compiler the policy uses.
pub fn validate_index_path_patterns(patterns: &[String]) -> Result<(), IndexPathPatternError> {
    compile(patterns).map(|_| ())
}

fn normalize(pattern: &str) -> &str {
    let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
    let pattern = pattern.trim_start_matches('/');
    pattern.strip_suffix('/').unwrap_or(pattern)
}

fn compile(patterns: &[String]) -> Result<GlobSet, IndexPathPatternError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(normalize(pattern))
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| IndexPathPatternError {
                pattern: pattern.clone(),
                message: error.kind().to_string(),
            })?;
        builder.add(glob);
    }
    builder.build().map_err(|error| IndexPathPatternError {
        pattern: patterns.join(", "),
        message: error.to_string(),
    })
}

fn matches_path_or_parent(set: &GlobSet, logical_path: &str) -> bool {
    if set.is_empty() {
        return false;
    }
    let mut candidate = logical_path;
    loop {
        if set.is_match(candidate) {
            return true;
        }
        match candidate.rfind('/') {
            Some(index) => candidate = &candidate[..index],
            None => return false,
        }
    }
}

fn literal_prefix(pattern: &str) -> Option<String> {
    let literal = pattern
        .split('/')
        .take_while(|segment| !segment.contains(['*', '?', '[', '{', '\\']))
        .collect::<Vec<_>>();
    (!literal.is_empty()).then(|| literal.join("/"))
}

#[cfg(test)]
mod tests {
    use super::IndexPathPolicyV1;

    fn policy(exclude: &[&str], include: &[&str]) -> IndexPathPolicyV1 {
        IndexPathPolicyV1::new(
            exclude.iter().map(|value| (*value).to_owned()).collect(),
            include.iter().map(|value| (*value).to_owned()).collect(),
        )
        .expect("valid patterns")
    }

    #[test]
    fn exclude_matches_paths_and_their_parent_directories() {
        let policy = policy(
            &["generated-fixtures/**", "docs", "**/*.min.*", "bin/**"],
            &[],
        );
        assert!(policy.excludes("generated-fixtures/gen.rs"));
        assert!(policy.excludes("generated-fixtures/deep/gen.rs"));
        assert!(policy.excludes("docs/guide/intro.md"));
        assert!(policy.excludes("assets/app.min.js"));
        assert!(policy.excludes("app.min.css"));
        assert!(policy.excludes("bin/run.rs"));
        assert!(!policy.excludes("src/bin/run.rs"));
        assert!(!policy.excludes("src/generated-fixtures.rs"));
        assert!(!policy.excludes("src/lib.rs"));
    }

    #[test]
    fn single_star_stays_inside_one_segment() {
        let policy = policy(&["src/*.rs"], &[]);
        assert!(policy.excludes("src/lib.rs"));
        assert!(!policy.excludes("src/nested/lib.rs"));
    }

    #[test]
    fn include_re_admits_what_exclude_drops() {
        let policy = policy(&["vendor/**"], &["vendor/kept/**"]);
        assert!(policy.excludes("vendor/other/lib.rs"));
        assert!(!policy.excludes("vendor/kept/lib.rs"));
        assert!(!policy.excludes("src/lib.rs"));
        assert!(policy.excludes_directory("vendor/other"));
        assert!(!policy.excludes_directory("vendor/kept"));
        assert!(
            !policy.excludes_directory("vendor"),
            "vendor/kept lies under vendor, so vendor must stay walkable"
        );
    }

    #[test]
    fn a_double_star_suffix_prunes_the_directory_it_names() {
        let policy = policy(&["target/**", "**/node_modules/**"], &[]);
        assert!(policy.excludes_directory("target"));
        assert!(policy.excludes_directory("node_modules"));
        assert!(policy.excludes_directory("web/node_modules"));
        assert!(!policy.excludes_directory("src"));
        assert!(!policy.excludes_directory("src/target_helpers"));
        assert!(
            !policy.excludes("target"),
            "a file named target is not under target/"
        );
    }

    #[test]
    fn a_leading_wildcard_include_keeps_every_excluded_directory_walkable() {
        let policy = policy(&["vendor/**"], &["**/keep.rs"]);
        assert!(!policy.excludes("vendor/a/keep.rs"));
        assert!(policy.excludes("vendor/a/drop.rs"));
        assert!(!policy.excludes_directory("vendor/a"));
    }

    #[test]
    fn a_malformed_pattern_is_a_typed_error_naming_it() {
        let error = IndexPathPolicyV1::new(vec!["src/[abc".to_owned()], Vec::new())
            .expect_err("unclosed class must not compile");
        assert_eq!(error.pattern, "src/[abc");
    }
}
