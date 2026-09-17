use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static REPOSITORY_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Returns the checked-out TraceDecay repository root for integration assets.
///
/// The `tracedecay` package lives below `crates/`, while fixtures, plugin
/// sources, and the dashboard remain repository-owned. Resolve that boundary
/// once and validate stable sentinels before any test consumes a checkout-only
/// path.
pub fn repository_root() -> &'static Path {
    REPOSITORY_ROOT
        .get_or_init(|| {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            assert!(
                root.join("Cargo.toml").is_file(),
                "missing workspace manifest"
            );
            assert!(root.join("dashboard").is_dir(), "missing dashboard source");
            assert!(root.join("plugin").is_dir(), "missing plugin source");
            root
        })
        .as_path()
}

pub fn repository_path(relative: impl AsRef<Path>) -> PathBuf {
    repository_root().join(relative)
}

#[test]
fn repository_root_resolves_workspace_asset_sentinels() {
    let root = repository_root();
    assert!(root.join("Cargo.toml").is_file());
    assert!(root.join("dashboard").is_dir());
    assert!(root.join("plugin").is_dir());
}
