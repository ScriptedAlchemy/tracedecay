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
