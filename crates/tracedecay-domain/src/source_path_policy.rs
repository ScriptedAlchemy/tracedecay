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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_source_policy_distinguishes_dependency_trees_from_source() {
        for generated in ["node_modules", ".venv", "dist", "target", "vendor"] {
            assert!(is_generated_dir_segment(generated), "{generated}");
        }
        for source in ["src", "tests", "packages", "builder"] {
            assert!(!is_generated_dir_segment(source), "{source}");
        }
    }
}
