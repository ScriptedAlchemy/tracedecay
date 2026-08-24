//! Crate-relative libtest identity derivation for managed affected-test runs.
//!
//! A discovered test node carries the qualified name the extractor observed
//! inside one file: `<file path>::<in-file module chain>::<test>`. Cargo's
//! `--exact` filter matches a different identity — the path relative to the
//! *test binary*, which additionally carries the module chain the file itself
//! contributes to the crate (`src/auth/login.rs` -> `auth::login`). Dispatching
//! the in-file suffix alone makes `cargo test` filter every test out and still
//! exit `0`, so a managed run executes nothing while reading as a success.
//!
//! `move_symbol::rust_paths::rust_module_path` answers a deliberately
//! different question: the `crate::`-rooted `use` path, which is defined for
//! every path shape. A libtest prefix must instead be *absent* for a Cargo
//! target root (`src/lib.rs`, `tests/<harness>.rs`) and absent for a layout
//! this authority cannot decide, so the two derivations are not interchangeable.

/// Directory names that root a Cargo compilation unit outside `src`. Each of
/// their immediate `.rs` children is its own crate root.
const AUXILIARY_TARGET_ROOTS: [&str; 3] = ["tests", "benches", "examples"];

/// Normalizes a stored path or qualified name to the `/`-separated,
/// leading-`./`-free form the graph writes for project-relative paths.
fn normalize(value: &str) -> String {
    let normalized = value.trim().replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_owned()
}

/// The module chain a Rust source file contributes to its test binary, or
/// `None` when the file is itself a target root or its layout is undecidable.
///
/// `src/auth/login.rs` -> `auth::login`, `src/auth/mod.rs` -> `auth`,
/// `crates/pkg/src/spool/tests.rs` -> `spool::tests`, `src/lib.rs` -> `None`,
/// `tests/harness.rs` -> `None`, `tests/harness/support.rs` -> `support`.
pub(super) fn libtest_module_prefix(file_path: &str) -> Option<String> {
    let normalized = normalize(file_path);
    let stem = normalized.strip_suffix(".rs")?;
    let segments: Vec<&str> = stem.split('/').filter(|part| !part.is_empty()).collect();
    // The final segment is the file stem, never the source root it lives under.
    let directories = segments.len().checked_sub(1)?;
    let directories = &segments[..directories];
    // `src` always wins: a `tests` directory below it (`src/a/tests/b.rs`) is
    // an ordinary module, not a Cargo target root.
    let source_root = directories.iter().rposition(|segment| *segment == "src");
    let root_index = match source_root {
        Some(index) => index,
        None => directories
            .iter()
            .rposition(|segment| AUXILIARY_TARGET_ROOTS.contains(segment))?,
    };
    let mut chain: Vec<&str> = segments[root_index + 1..].to_vec();

    if source_root.is_some() {
        // `src/bin/<name>.rs` and `src/bin/<name>/main.rs` are their own crate
        // roots, so neither `bin` nor the binary's name is a module segment.
        if chain.first() == Some(&"bin") {
            chain.drain(..2.min(chain.len()));
        }
    } else {
        // `tests/<harness>.rs` is a crate root; `tests/<harness>/a.rs` is its
        // module `a`. Either way the harness name is not a module segment.
        chain.drain(..1.min(chain.len()));
    }

    match chain.last().copied() {
        // `foo/mod.rs` is the module `foo`, whatever the depth.
        Some("mod") => {
            chain.pop();
        }
        // A lone `lib`/`main` is the crate root file, not a module named so.
        Some("lib" | "main") if chain.len() == 1 => {
            chain.pop();
        }
        _ => {}
    }

    (!chain.is_empty()).then(|| chain.join("::"))
}

/// The exact libtest identity for a discovered test node.
///
/// Returns `None` when the stored qualified name does not carry the node's own
/// file path, which is the only shape this authority can decide.
pub(super) fn libtest_identity(file_path: &str, qualified_name: &str) -> Option<String> {
    let file_path_prefix = format!("{}::", normalize(file_path));
    let in_file_chain = normalize(qualified_name)
        .strip_prefix(&file_path_prefix)?
        .to_owned();
    if in_file_chain.is_empty() {
        return None;
    }
    match libtest_module_prefix(file_path) {
        Some(module_prefix) => Some(format!("{module_prefix}::{in_file_chain}")),
        None => Some(in_file_chain),
    }
}

#[cfg(test)]
mod tests {
    use super::{libtest_identity, libtest_module_prefix};

    #[test]
    fn source_files_contribute_their_module_chain() {
        assert_eq!(
            libtest_module_prefix("src/auth/login.rs").as_deref(),
            Some("auth::login")
        );
        assert_eq!(
            libtest_module_prefix("src/auth/mod.rs").as_deref(),
            Some("auth")
        );
        assert_eq!(
            libtest_module_prefix("src/auth/tests/login_flow.rs").as_deref(),
            Some("auth::tests::login_flow"),
            "a `tests` directory below `src` is an ordinary module, not a target root"
        );
        assert_eq!(
            libtest_module_prefix("crates/tracedecay-hooks/src/spool/tests.rs").as_deref(),
            Some("spool::tests")
        );
        assert_eq!(
            libtest_module_prefix("./src/a/b/c/mod.rs").as_deref(),
            Some("a::b::c")
        );
        assert_eq!(
            libtest_module_prefix("src\\auth\\login.rs").as_deref(),
            Some("auth::login"),
            "stored Windows separators must not defeat the module chain"
        );
    }

    #[test]
    fn crate_root_files_contribute_no_module_chain() {
        assert_eq!(libtest_module_prefix("src/lib.rs"), None);
        assert_eq!(libtest_module_prefix("src/main.rs"), None);
        assert_eq!(libtest_module_prefix("crates/pkg/src/lib.rs"), None);
        assert_eq!(libtest_module_prefix("src/bin/tool.rs"), None);
        assert_eq!(libtest_module_prefix("src/bin/tool/main.rs"), None);
        assert_eq!(
            libtest_module_prefix("src/bin/tool/support.rs").as_deref(),
            Some("support")
        );
        // A module genuinely named `main` below the crate root is kept.
        assert_eq!(
            libtest_module_prefix("src/daemon/main.rs").as_deref(),
            Some("daemon::main")
        );
    }

    #[test]
    fn harness_roots_contribute_no_module_chain() {
        assert_eq!(libtest_module_prefix("tests/edited_only.rs"), None);
        assert_eq!(libtest_module_prefix("benches/throughput.rs"), None);
        assert_eq!(libtest_module_prefix("examples/demo.rs"), None);
        assert_eq!(
            libtest_module_prefix("tests/harness/support.rs").as_deref(),
            Some("support")
        );
        assert_eq!(
            libtest_module_prefix("tests/harness/support/mod.rs").as_deref(),
            Some("support")
        );
        assert_eq!(libtest_module_prefix("tests/harness/main.rs"), None);
    }

    #[test]
    fn undecidable_paths_contribute_no_module_chain() {
        assert_eq!(libtest_module_prefix("README.md"), None);
        assert_eq!(libtest_module_prefix("build.rs"), None);
        assert_eq!(libtest_module_prefix("vendor/foo/lib.rs"), None);
    }

    #[test]
    fn identity_carries_the_file_module_path_before_the_in_file_chain() {
        assert_eq!(
            libtest_identity(
                "src/auth/login.rs",
                "src/auth/login.rs::tests::successful_login_creates_session"
            )
            .as_deref(),
            Some("auth::login::tests::successful_login_creates_session")
        );
        assert_eq!(
            libtest_identity(
                "tests/edited_only.rs",
                "tests/edited_only.rs::nested::first"
            )
            .as_deref(),
            Some("nested::first")
        );
    }

    #[test]
    fn identity_is_undecidable_without_the_stored_file_prefix() {
        assert_eq!(
            libtest_identity("src/auth/login.rs", "auth::login::tests::orphaned"),
            None
        );
        assert_eq!(
            libtest_identity("src/auth/login.rs", "src/auth/login.rs::"),
            None
        );
    }
}
