//! Project file scanning: walking the tree, include/skip filtering, and
//! index-coverage hints.

use std::collections::HashSet;
use std::path::{Component, Path};

use walkdir::WalkDir;

use crate::config::{TraceDecayConfig, is_excluded, is_excluded_dir, is_included, is_included_dir};
use crate::types::*;

use super::TraceDecay;

fn normalize_include_folder(project_root: &Path, folder: &str) -> String {
    let trimmed = folder.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let raw_path = Path::new(trimmed);
    let project_relative = if raw_path.is_absolute() {
        raw_path.strip_prefix(project_root).unwrap_or(raw_path)
    } else {
        raw_path
    };

    let mut parts = Vec::new();
    for component in project_relative.components() {
        match component {
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().replace('\\', "/")),
            Component::ParentDir => parts.push("..".to_string()),
        }
    }

    parts.join("/").trim_matches('/').to_string()
}

fn include_may_match_descendant(dir_path: &str, config: &TraceDecayConfig) -> bool {
    if is_included_dir(dir_path, config) || is_included(dir_path, config) {
        return true;
    }

    let dir_prefix = format!("{}/", dir_path.trim_end_matches('/'));
    for pattern in &config.include {
        let normalized = pattern.trim_start_matches('/').replace('\\', "/");
        if normalized.starts_with("**/") {
            return true;
        }
        if normalized.starts_with(&dir_prefix) {
            return true;
        }
        let literal_prefix = normalized
            .split(['*', '?', '['])
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        if !literal_prefix.is_empty()
            && (literal_prefix == dir_path || literal_prefix.starts_with(&dir_prefix))
        {
            return true;
        }
    }

    false
}

fn is_nested_repository_root(path: &Path, depth: usize, is_dir: bool) -> bool {
    depth > 0 && is_dir && path.join(".git").exists()
}

impl TraceDecay {
    /// Appends runtime skip-folder patterns to the exclude list.
    ///
    /// Each folder name is converted to a `folder/**` glob so that all
    /// files underneath it are excluded during scanning.
    pub fn add_skip_folders(&mut self, folders: &[String]) {
        for folder in folders {
            self.config.exclude.push(format!("{folder}/**"));
        }
    }

    /// Appends runtime include-folder patterns to the include list.
    ///
    /// Includes are explicit user intent and override the built-in generated
    /// folder/gitignore filters for the requested subtree.
    pub fn add_include_folders(&mut self, folders: &[String]) {
        for folder in folders {
            let normalized = normalize_include_folder(&self.project_root, folder);
            if !normalized.is_empty() {
                self.config.include.push(format!("{normalized}/**"));
            }
        }
    }

    /// Scans the project root for source files in all supported languages,
    /// respecting the configured exclude patterns and max file size.
    ///
    /// When `git_ignore` is enabled in the config, `.gitignore` rules are
    /// applied via the `ignore` crate. Otherwise, hidden directories and
    /// `target/` are skipped with a simple name-based filter.
    ///
    /// Supported extensions are derived from the `LanguageRegistry` so that
    /// adding a new extractor automatically picks up its files.
    pub(super) fn scan_files(&self) -> Vec<String> {
        debug_assert!(
            self.project_root.is_dir(),
            "scan_files: project_root is not a directory"
        );
        let supported_exts = self.registry.supported_extensions();
        debug_assert!(
            !supported_exts.is_empty(),
            "scan_files: no supported extensions registered"
        );

        if self.config.git_ignore {
            let files = self.scan_files_with_gitignore(&supported_exts);
            if files.is_empty() {
                // The project directory may be gitignored by a parent repo,
                // causing the ignore-aware walker to skip everything. Fall
                // back to plain walkdir if source files clearly exist.
                let has_source = WalkDir::new(&self.project_root)
                    .follow_links(true)
                    .max_depth(2)
                    .into_iter()
                    .filter_entry(|entry| {
                        !is_nested_repository_root(
                            entry.path(),
                            entry.depth(),
                            entry.file_type().is_dir(),
                        )
                    })
                    .filter_map(std::result::Result::ok)
                    .any(|e| {
                        e.file_type().is_file()
                            && e.path()
                                .extension()
                                .and_then(|ext| ext.to_str())
                                .is_some_and(|ext| supported_exts.contains(&ext))
                    });
                if has_source {
                    eprintln!(
                        "warning: gitignore-aware scan found no files; falling back to plain walk (project may be gitignored by parent repo)"
                    );
                    return self.scan_files_walkdir(&supported_exts);
                }
            }
            files
        } else {
            self.scan_files_walkdir(&supported_exts)
        }
    }

    /// Walk using `walkdir`, skipping hidden directories and `target/`.
    ///
    /// Hidden (dot-prefixed) entries that match a configured `include` glob
    /// are allowed through despite the default filter.
    fn scan_files_walkdir(&self, supported_exts: &[&str]) -> Vec<String> {
        let mut files = Vec::new();
        let root = &self.project_root;
        let config = &self.config;
        for entry in WalkDir::new(root)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                if is_nested_repository_root(e.path(), e.depth(), e.file_type().is_dir()) {
                    return false;
                }
                let name = e.file_name().to_string_lossy();
                if name.starts_with('.') || name == "target" {
                    // Allow if the relative path matches an include glob.
                    if let Ok(rel) = e.path().strip_prefix(root) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        return include_may_match_descendant(&rel_str, config);
                    }
                    return false;
                }
                // Prune directories covered by an exclude glob before descending.
                // This prevents entering large trees (e.g. node_modules) and
                // avoids following symlinks that cycle back into source directories.
                if e.file_type().is_dir()
                    && let Ok(rel) = e.path().strip_prefix(root)
                {
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    if is_excluded_dir(&rel_str, config)
                        && !include_may_match_descendant(&rel_str, config)
                    {
                        return false;
                    }
                }
                true
            })
        {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let metadata = entry.metadata().ok();
            if let Some(rel_str) = self.accept_file(entry.path(), supported_exts, metadata.as_ref())
            {
                files.push(rel_str);
            }
        }
        files
    }

    /// Walk using the `ignore` crate, which respects `.gitignore` rules,
    /// `.git/info/exclude`, and the user's global gitignore.
    ///
    /// `git_ignore(true)` alone only reads nested `.gitignore` files when a
    /// `.git` directory is reachable from the walk root (it relies on git repo
    /// discovery). `add_custom_ignore_filename(".gitignore")` makes the crate
    /// additionally treat every `.gitignore` it encounters as a standalone
    /// ignore file, ensuring nested rules are applied even outside a git repo.
    ///
    /// When `include` globs are configured, the crate's built-in hidden filter
    /// is disabled and hidden entries are filtered manually so that included
    /// dot-paths can pass through.
    fn scan_files_with_gitignore(&self, supported_exts: &[&str]) -> Vec<String> {
        let has_includes = !self.config.include.is_empty();
        let mut files = Vec::new();
        let walker = ignore::WalkBuilder::new(&self.project_root)
            .follow_links(true)
            .hidden(!has_includes) // disable when we need to check includes
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .add_custom_ignore_filename(".gitignore")
            .filter_entry(|entry| {
                !is_nested_repository_root(
                    entry.path(),
                    entry.depth(),
                    entry
                        .file_type()
                        .is_some_and(|file_type| file_type.is_dir()),
                )
            })
            .build();

        for entry in walker {
            let Ok(entry) = entry else { continue };
            let Some(ft) = entry.file_type() else {
                continue;
            };

            // When we disabled the crate's hidden filter, manually skip hidden
            // entries that don't match an include glob.
            if has_includes && entry.depth() > 0 {
                let name = entry.file_name().to_string_lossy();
                if name.starts_with('.') {
                    if let Ok(rel) = entry.path().strip_prefix(&self.project_root) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        if !is_included(&rel_str, &self.config)
                            && !is_included_dir(&rel_str, &self.config)
                        {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
            }

            if !ft.is_file() {
                continue;
            }
            let metadata = entry.metadata().ok();
            if let Some(rel_str) = self.accept_file(entry.path(), supported_exts, metadata.as_ref())
            {
                files.push(rel_str);
            }
        }
        if has_includes {
            let mut seen: HashSet<String> = files.iter().cloned().collect();
            for rel_str in self.scan_included_files(supported_exts) {
                if seen.insert(rel_str.clone()) {
                    files.push(rel_str);
                }
            }
        }
        files
    }

    /// Walk all files and add only explicit include matches. This lets
    /// `include` pierce gitignore/default excludes without turning the whole
    /// ignore-aware walker into an include-only walk.
    fn scan_included_files(&self, supported_exts: &[&str]) -> Vec<String> {
        let mut files = Vec::new();
        if self.config.include.is_empty() {
            return files;
        }

        for entry in WalkDir::new(&self.project_root)
            .follow_links(true)
            .into_iter()
            .filter_entry(|entry| {
                if entry.depth() == 0 || !entry.file_type().is_dir() {
                    return true;
                }
                if is_nested_repository_root(entry.path(), entry.depth(), true) {
                    return false;
                }
                let Ok(rel) = entry.path().strip_prefix(&self.project_root) else {
                    return true;
                };
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                include_may_match_descendant(&rel_str, &self.config)
            })
        {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let metadata = entry.metadata().ok();
            if let Some(rel_str) = self.accept_file(entry.path(), supported_exts, metadata.as_ref())
                && is_included(&rel_str, &self.config)
            {
                files.push(rel_str);
            }
        }
        files
    }

    /// Checks whether a file should be included: correct extension, not
    /// excluded by config globs, and within the max file size.
    ///
    /// `metadata` should be the entry's already-stat'd metadata from the
    /// walker (`walkdir::DirEntry::metadata` / `ignore::DirEntry::metadata`)
    /// when the caller has one, so this doesn't re-stat a file the walker
    /// already stat'd to determine its file type. Falls back to a fresh
    /// `std::fs::metadata` call when the caller has none (or the walker's
    /// own stat failed).
    fn accept_file(
        &self,
        path: &Path,
        supported_exts: &[&str],
        metadata: Option<&std::fs::Metadata>,
    ) -> Option<String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !supported_exts.contains(&ext) {
            return None;
        }
        let relative = path.strip_prefix(&self.project_root).ok()?;
        // Normalize to forward slashes so paths are consistent across
        // platforms and between different directory walkers on Windows.
        let rel_str = relative.to_string_lossy().replace('\\', "/");
        if is_excluded(&rel_str, &self.config) && !is_included(&rel_str, &self.config) {
            return None;
        }
        let len = match metadata {
            Some(m) => m.len(),
            None => std::fs::metadata(path).ok()?.len(),
        };
        if len > self.config.max_file_size {
            return None;
        }
        Some(rel_str)
    }

    /// Returns a low-noise hint when a graph lookup may be incomplete because
    /// generated/vendor/cache folders are intentionally excluded from the index.
    pub fn index_coverage_hint(&self, result_count: usize) -> Option<IndexCoverageHint> {
        if result_count > 0 {
            return None;
        }

        let skipped_dirs = self.existing_skipped_dirs(5);
        if skipped_dirs.is_empty() {
            return None;
        }

        let first = skipped_dirs[0].clone();
        let message = if self.config.git_ignore {
            "No indexed symbols matched. This project respects .gitignore and default generated/vendor folder skips; the target may be under a skipped tree."
        } else {
            "No indexed symbols matched. This project uses default generated/vendor folder skips; the target may be under a skipped tree."
        };

        Some(IndexCoverageHint {
            message: message.to_string(),
            skipped_dirs,
            suggested_command: format!("tracedecay sync --include-folder {first}"),
        })
    }

    fn existing_skipped_dirs(&self, limit: usize) -> Vec<String> {
        let mut dirs = Vec::new();
        let mut seen = HashSet::new();
        let root = &self.project_root;

        for entry in WalkDir::new(root).follow_links(false).max_depth(5) {
            if dirs.len() >= limit {
                break;
            }
            let Ok(entry) = entry else {
                continue;
            };
            if entry.depth() == 0 || !entry.file_type().is_dir() {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else {
                continue;
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let name = entry.file_name().to_string_lossy();
            if Self::is_skipped_dir_hint(&rel_str, &name, &self.config)
                && seen.insert(rel_str.clone())
            {
                dirs.push(rel_str);
            }
        }

        dirs
    }

    /// Informational only: the authoritative skip decision during indexing
    /// is [`is_excluded_dir`]/[`is_excluded`] against the (possibly
    /// user-overridden) `config.exclude` patterns. This hint just decides
    /// which *already-excluded* directories are common enough generated/
    /// vendored names to be worth surfacing in [`IndexCoverageHint`], so it
    /// delegates the "is this name generated?" question to the shared
    /// [`crate::config::is_generated_dir_segment`] list rather than
    /// hand-maintaining its own copy.
    fn is_skipped_dir_hint(rel_str: &str, name: &str, config: &TraceDecayConfig) -> bool {
        if is_included_dir(rel_str, config) || is_included(rel_str, config) {
            return false;
        }

        crate::config::is_generated_dir_segment(name) && is_excluded_dir(rel_str, config)
    }
}

#[cfg(test)]
mod skipped_dir_hint_tests {
    use super::TraceDecay;
    use crate::config::TraceDecayConfig;

    #[test]
    fn hints_on_excluded_generated_dirs() {
        let config = TraceDecayConfig::default();
        assert!(TraceDecay::is_skipped_dir_hint(
            "node_modules",
            "node_modules",
            &config
        ));
        assert!(TraceDecay::is_skipped_dir_hint("dist", "dist", &config));
    }

    #[test]
    fn gains_segments_previously_absent_from_its_own_hintable_list() {
        // "target" and ".worktrees" weren't in scan.rs's old hand-maintained
        // HINTABLE_DIRS but are part of the shared GENERATED_DIR_SEGMENTS
        // union other call sites already recognized.
        let config = TraceDecayConfig::default();
        assert!(TraceDecay::is_skipped_dir_hint("target", "target", &config));
        assert!(TraceDecay::is_skipped_dir_hint(
            ".worktrees",
            ".worktrees",
            &config
        ));
    }

    #[test]
    fn does_not_hint_on_real_source_dirs() {
        let config = TraceDecayConfig::default();
        assert!(!TraceDecay::is_skipped_dir_hint("src", "src", &config));
    }

    #[test]
    fn explicit_include_overrides_the_hint() {
        let config = TraceDecayConfig {
            include: vec!["dist/**".to_string()],
            ..TraceDecayConfig::default()
        };
        assert!(!TraceDecay::is_skipped_dir_hint("dist", "dist", &config));
    }
}
