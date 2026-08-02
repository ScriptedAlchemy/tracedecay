// This file is compiled both as a module of this crate and, through a
// `#[path]` module in build.rs, as part of the build script — the same
// arrangement `src/version/build_identity.rs` uses in the root crate. It
// therefore carries no inner doc comments and no `use` statements, so the one
// parser bakes the value and the one parser verifies it.

/// File name of the workspace-root manifest that owns the product version.
pub const ROOT_MANIFEST_FILE: &str = "Cargo.toml";

/// Name of the root package whose `version` is the TraceDecay product version.
pub const ROOT_PACKAGE_NAME: &str = "tracedecay";

/// Path of the workspace-root manifest below `repo_root`.
///
/// Handed to Cargo as a `rerun-if-changed` trigger so a release bump of the
/// root package rebuilds this crate instead of leaving a stale version baked
/// into plugin manifests and cache paths.
pub fn manifest_path(repo_root: &std::path::Path) -> std::path::PathBuf {
    repo_root.join(ROOT_MANIFEST_FILE)
}

/// The root package's version, read from the workspace-root manifest below
/// `repo_root`.
///
/// `None` when the manifest is missing or unreadable, when it does not declare
/// the expected root package, or when that package has no literal `version`.
/// Every one of those means the value this crate would otherwise stamp is not
/// the product version, so callers must fail loudly rather than substitute
/// their own `CARGO_PKG_VERSION` — that substitution is precisely the silent
/// drift this module exists to prevent.
pub fn resolve(repo_root: &std::path::Path) -> Option<String> {
    let manifest = std::fs::read_to_string(manifest_path(repo_root)).ok()?;
    if package_field(&manifest, "name")? != ROOT_PACKAGE_NAME {
        return None;
    }
    Some(package_field(&manifest, "version")?.to_string())
}

/// The first literal string assigned to `key` inside the manifest's `[package]`
/// table.
///
/// Deliberately a line scanner rather than a TOML parse: this crate's build
/// script has no build dependencies, and the two fields it needs are plain
/// basic strings in a hand-maintained manifest. Inherited values
/// (`version.workspace = true`) are not literals and are reported as absent,
/// which the caller surfaces as a failure instead of a wrong stamp.
pub fn package_field<'a>(manifest: &'a str, key: &str) -> Option<&'a str> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        // `version.workspace = true` and `versions = …` both start with the
        // key; only a bare `key =` assignment is the field we want.
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        // A non-literal value (an inherited or computed field) is reported as
        // absent rather than searched past: the assignment was found, and any
        // later match would belong to a different key.
        let rest = rest.trim_start().strip_prefix('"')?;
        return rest.split('"').next();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{ROOT_PACKAGE_NAME, package_field, resolve};

    const MANIFEST: &str = "\
[workspace]
members = [\"crates/tracedecay-agent-hosts\"]

[workspace.package]
edition = \"2024\"
version = \"9.9.9\"

[package]
name = \"tracedecay\"
version = \"0.0.67\"
edition.workspace = true
# version = \"0.0.1\"
include = [\"src/**\"]

[dependencies]
version = \"1\"
";

    #[test]
    fn the_package_table_wins_over_every_other_table() {
        assert_eq!(package_field(MANIFEST, "version"), Some("0.0.67"));
        assert_eq!(package_field(MANIFEST, "name"), Some(ROOT_PACKAGE_NAME));
    }

    #[test]
    fn an_inherited_version_is_not_a_literal_this_can_stamp() {
        let manifest = "[package]\nname = \"tracedecay\"\nversion.workspace = true\n";
        assert_eq!(package_field(manifest, "version"), None);
    }

    #[test]
    fn a_key_that_is_only_a_prefix_is_not_the_field() {
        let manifest = "[package]\nversions = \"nope\"\nversion = \"1.2.3\"\n";
        assert_eq!(package_field(manifest, "version"), Some("1.2.3"));
    }

    #[test]
    fn a_manifest_without_a_package_table_has_no_product_version() {
        assert_eq!(
            package_field("[workspace]\nversion = \"1\"\n", "version"),
            None
        );
    }

    #[test]
    fn a_tree_without_the_root_manifest_resolves_to_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(resolve(dir.path()), None);
    }

    /// A crate directory is not the workspace root; resolving against the wrong
    /// root must report nothing rather than some other package's version.
    #[test]
    fn a_manifest_for_a_different_package_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"tracedecay-agent-hosts\"\nversion = \"0.1.0\"\n",
        )
        .expect("write manifest");
        assert_eq!(resolve(dir.path()), None);
    }

    #[test]
    fn the_root_package_version_round_trips_through_a_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), MANIFEST).expect("write manifest");
        assert_eq!(resolve(dir.path()).as_deref(), Some("0.0.67"));
    }
}
