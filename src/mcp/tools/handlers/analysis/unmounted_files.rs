//! `tracedecay_unmounted_files` — source files on disk that nothing declares.
//!
//! A compiler only ever sees a file that something else reaches: Rust follows
//! `mod` declarations from a cargo target root, a bundler follows `import` and
//! `require` from a declared entry point. A file nobody reaches is invisible to
//! `cargo check`, to `tsc`, and to every test run — but it is fully visible to a
//! code-graph indexer, which walks the working tree rather than the module
//! tree. That asymmetry is the exact failure this report exists to name: seven
//! files under `src/daemon/` sat in this repository indexed as healthy-looking
//! symbols, with signatures and neighbours and apparent callers, while the
//! compiler had never parsed a line of them. Nothing in the graph could say so,
//! because the graph is built from the filesystem and the truth lives in the
//! reachability graph.
//!
//! The audit is per ecosystem, because "reachable" means something different in
//! each one and pretending otherwise would be the same lie in a new place:
//!
//!   - [`rust`] answers "which `.rs` files under a cargo package's own source
//!     directories are NOT reachable from its targets by following `mod`?".
//!     Unreachable there means the compiler genuinely never parses the file.
//!   - [`typescript`] answers "which source files are NOT reachable from any
//!     declared entry point by following static `import` / `require` /
//!     `export … from`?". Unreachable there is a *weaker* claim — `tsc` still
//!     type-checks anything matched by a tsconfig `include` — and the report
//!     says so in its own verdict line rather than borrowing Rust's certainty.
//!
//! Three deliberate choices keep the answer truthful rather than merely
//! plausible:
//!
//!   - **A conditional declaration counts as mounted.** `#[cfg(...)] mod x;`,
//!     `#[cfg_attr(unix, path = "…")]`, an `import` behind a bundler condition:
//!     the scan does not evaluate predicates and must not pretend to. Treating
//!     a gated module as an orphan would flood the answer with files that are
//!     working exactly as intended.
//!   - **Every root is its own root.** Each `tests/*.rs`, each
//!     `tests/<name>/main.rs`, each npm package in a monorepo starts a separate
//!     reachability walk. This is precisely the shape in which the daemon
//!     orphans hid, so the walk models it rather than treating a directory as
//!     one bag.
//!   - **A file the walk cannot claim is reported, never silently dropped.** A
//!     false positive costs a reader one look; a false negative costs a
//!     release. Ecosystems with no reachability model here are listed with an
//!     explicit `unsupported` status and a file count, so "no findings" can
//!     never be confused with "not looked at".

mod rust;
mod typescript;

use std::collections::BTreeMap;
use std::path::{Component, PathBuf};

use super::*;

/// Default and ceiling for reported orphans in one response.
///
/// Unlike the paged import scan there is no cursor: the whole reachability
/// graph must be walked to know that *any* file is unmounted, so a second page
/// would repeat the entire walk for a suffix of the same answer. The response
/// states the true total and how many rows it omitted instead of pretending the
/// returned list is the whole finding.
const UNMOUNTED_FILES_DEFAULT_LIMIT: usize = 200;
const UNMOUNTED_FILES_MAX_LIMIT: usize = 2_000;

/// One orphaned file and, where the ecosystem admits one, the smallest repair
/// that would mount it.
pub(super) struct UnmountedFile {
    pub(super) file: String,
    /// Package that owns the file — a crate name, an npm package name.
    pub(super) package: String,
    /// Project-relative manifest path, so a finding names something a reader
    /// can open rather than an absolute path from this machine.
    pub(super) manifest: String,
    /// The nearest ancestor that IS mounted and could declare this file.
    /// `None` means the whole branch is detached, which is a different and
    /// larger finding than one missing declaration.
    pub(super) nearest_mounted_parent: Option<String>,
    /// The exact line to add, when the ecosystem has one canonical repair.
    /// TypeScript has none: an unimported file is either dead or reached by a
    /// blind spot, and guessing an `import` line would invent a caller.
    pub(super) suggested_declaration: Option<String>,
}

/// Whether an ecosystem was audited, absent, or recognised but unmodelled.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum EcosystemStatus {
    /// A reachability walk ran and its findings are authoritative.
    Audited,
    /// No manifest or source file of this ecosystem exists in the project.
    NotPresent,
    /// Files of this ecosystem exist and no reachability model covers them.
    Unsupported,
}

impl EcosystemStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Audited => "audited",
            Self::NotPresent => "not_present",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One ecosystem's answer, including what its verdict does and does not claim.
pub(super) struct EcosystemAudit {
    pub(super) ecosystem: &'static str,
    pub(super) status: EcosystemStatus,
    pub(super) package_count: usize,
    pub(super) entry_point_count: usize,
    pub(super) scanned_file_count: usize,
    pub(super) mounted_file_count: usize,
    /// Files of this ecosystem's languages that no package claims — outside
    /// every package's source directories, so out of scope by construction.
    pub(super) unclaimed_file_count: usize,
    /// What "unmounted" asserts here, stated so a reader never has to assume
    /// it means the same thing it means for another ecosystem.
    pub(super) verdict: &'static str,
    /// Reachability this walk cannot see. Stated, never papered over.
    pub(super) blind_spots: Vec<&'static str>,
    pub(super) note: Option<String>,
    pub(super) excluded_globs: Vec<String>,
    pub(super) unmounted: Vec<UnmountedFile>,
}

impl EcosystemAudit {
    /// An ecosystem with no manifest and no source files in this project.
    pub(super) fn not_present(ecosystem: &'static str, verdict: &'static str) -> Self {
        Self {
            ecosystem,
            status: EcosystemStatus::NotPresent,
            package_count: 0,
            entry_point_count: 0,
            scanned_file_count: 0,
            mounted_file_count: 0,
            unclaimed_file_count: 0,
            verdict,
            blind_spots: Vec::new(),
            note: None,
            excluded_globs: Vec::new(),
            unmounted: Vec::new(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "ecosystem": self.ecosystem,
            "status": self.status.as_str(),
            "package_count": self.package_count,
            "entry_point_count": self.entry_point_count,
            "scanned_file_count": self.scanned_file_count,
            "mounted_file_count": self.mounted_file_count,
            "unclaimed_file_count": self.unclaimed_file_count,
            "unmounted_file_count": self.unmounted.len(),
            "verdict": self.verdict,
            "blind_spots": self.blind_spots,
            "note": self.note,
            "excluded_path_globs": self.excluded_globs,
        })
    }
}

/// The whole audit result, before argument-level filtering and paging.
struct ProjectAudit {
    ecosystems: Vec<EcosystemAudit>,
}

/// Every file in the working tree, walked once and shared by every ecosystem.
///
/// The walk is the same one the indexer and `tracedecay_grep` use (`.gitignore`
/// honoured, `target/`, `vendor/`, `node_modules/` and the other generated
/// directories skipped, links not followed). Reusing it rather than restating
/// its policy is the point: a scan that disagreed with the indexer's file set
/// would report findings nothing else in the product can see.
pub(super) struct ProjectFiles {
    root: PathBuf,
    files: Vec<PathBuf>,
}

impl ProjectFiles {
    fn collect(project_root: &Path) -> Result<Self> {
        let walk = tracedecay_code_index::source_walk::source_walk(project_root, None).map_err(
            |error| TraceDecayError::Config {
                message: format!("source walk rejected its own scope: {}", error.message),
            },
        )?;
        let mut files = Vec::new();
        for entry in walk {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            files.push(entry.into_path());
        }
        files.sort();
        Ok(Self {
            root: project_root.to_path_buf(),
            files,
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Every walked file whose extension is one of `extensions`
    /// (case-insensitively, without the leading dot).
    pub(super) fn with_extensions(&self, extensions: &[&str]) -> Vec<&Path> {
        self.files
            .iter()
            .filter(|path| {
                path.extension().is_some_and(|found| {
                    extensions
                        .iter()
                        .any(|wanted| found.eq_ignore_ascii_case(wanted))
                })
            })
            .map(PathBuf::as_path)
            .collect()
    }

    /// Every walked file with this exact file name — the manifest sweep both
    /// ecosystems use to find packages nobody declared.
    pub(super) fn named(&self, file_name: &str) -> Vec<&Path> {
        self.files
            .iter()
            .filter(|path| path.file_name().is_some_and(|name| name == file_name))
            .map(PathBuf::as_path)
            .collect()
    }

    /// A finding names a project-relative path with forward slashes, on every
    /// platform, so the same repository reads the same way everywhere.
    pub(super) fn relative(&self, path: &Path) -> String {
        relative_display(&self.root, path)
    }
}

/// Project-relative, forward-slashed rendering of `path`.
pub(super) fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Lexical `.`/`..` normalization.
///
/// `#[path = "../shared.rs"]` and `import '../shared'` are both legal and both
/// produce a path that would never compare equal to the walker's own form of
/// the same file, so both sides are normalized before they meet in the mounted
/// set. Symlinks are deliberately not resolved: the walker does not follow them
/// either, and resolving here would make the two sets disagree again.
pub(super) fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Handles `tracedecay_unmounted_files` tool calls.
pub(crate) async fn handle_unmounted_files(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_unmounted_files")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(UNMOUNTED_FILES_DEFAULT_LIMIT, |limit| {
            (limit as usize).clamp(1, UNMOUNTED_FILES_MAX_LIMIT)
        });
    let path_filter = effective_path(&args, scope_prefix).map(str::to_owned);
    let ecosystem_filter = args
        .get("ecosystem")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);

    let project_root = cg.project_root().to_path_buf();
    // The walk reads every candidate source file, so it runs on a blocking
    // worker rather than holding the async dispatch thread through thousands
    // of synchronous reads.
    let audit = tokio::task::spawn_blocking(move || audit_project(&project_root))
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("unmounted-file audit did not complete: {error}"),
        })??;

    let matching = audit
        .ecosystems
        .iter()
        .filter(|ecosystem| {
            ecosystem_filter
                .as_deref()
                .is_none_or(|wanted| wanted == ecosystem.ecosystem)
        })
        .flat_map(|ecosystem| {
            ecosystem
                .unmounted
                .iter()
                .map(move |entry| (ecosystem.ecosystem, entry))
        })
        .filter(|(_, entry)| {
            crate::path_scope::path_matches_scope(&entry.file, path_filter.as_deref())
        })
        .collect::<Vec<_>>();
    let unmounted_file_count = matching.len();
    let returned = matching.iter().take(limit).collect::<Vec<_>>();
    let touched_files = unique_file_paths(returned.iter().map(|(_, entry)| entry.file.as_str()));

    let rows = returned
        .iter()
        .map(|(ecosystem, entry)| {
            json!({
                "file": entry.file,
                "ecosystem": ecosystem,
                "package": entry.package,
                "manifest": entry.manifest,
                "nearest_mounted_parent": entry.nearest_mounted_parent,
                "suggested_declaration": entry.suggested_declaration,
            })
        })
        .collect::<Vec<_>>();

    let output = json!({
        "unmounted_file_count": unmounted_file_count,
        "returned_count": rows.len(),
        "omitted_count": unmounted_file_count.saturating_sub(rows.len()),
        "complete": rows.len() == unmounted_file_count,
        "ecosystems": audit
            .ecosystems
            .iter()
            .map(EcosystemAudit::to_json)
            .collect::<Vec<_>>(),
        "limit": limit,
        "path": path_filter,
        "ecosystem": ecosystem_filter,
        "unmounted": rows,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::unmounted_files_md(&output),
    ))
}

/// Walks the working tree once and asks each ecosystem its own question.
fn audit_project(project_root: &Path) -> Result<ProjectAudit> {
    let files = ProjectFiles::collect(project_root)?;
    let mut ecosystems = vec![rust::audit(&files)?, typescript::audit(&files)];
    ecosystems.extend(unmodelled_ecosystems(&files));
    Ok(ProjectAudit { ecosystems })
}

/// Languages present in the tree that this audit has no reachability model for.
///
/// Reporting them as `unsupported` with a file count is the whole point: a
/// caller who points the audit at a Go service must not read "no findings" as
/// "no orphans". Silence is the one answer a truthfulness tool may not give.
fn unmodelled_ecosystems(files: &ProjectFiles) -> Vec<EcosystemAudit> {
    /// Extension → ecosystem label, for languages the graph indexes but this
    /// audit cannot walk. Rust and the TypeScript family are absent because
    /// they are modelled above.
    const UNMODELLED: [(&str, &str); 24] = [
        ("py", "python"),
        ("go", "go"),
        ("rb", "ruby"),
        ("java", "java"),
        ("kt", "kotlin"),
        ("kts", "kotlin"),
        ("cs", "csharp"),
        ("php", "php"),
        ("swift", "swift"),
        ("scala", "scala"),
        ("dart", "dart"),
        ("ex", "elixir"),
        ("exs", "elixir"),
        ("erl", "erlang"),
        ("hs", "haskell"),
        ("ml", "ocaml"),
        ("clj", "clojure"),
        ("lua", "lua"),
        ("zig", "zig"),
        ("c", "c"),
        ("h", "c"),
        ("cpp", "cpp"),
        ("hpp", "cpp"),
        ("cc", "cpp"),
    ];

    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for file in &files.files {
        let Some(extension) = file.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        let lowered = extension.to_ascii_lowercase();
        if let Some((_, ecosystem)) = UNMODELLED
            .iter()
            .find(|(candidate, _)| *candidate == lowered)
        {
            *counts.entry(*ecosystem).or_default() += 1;
        }
    }

    counts
        .into_iter()
        .map(|(ecosystem, count)| EcosystemAudit {
            ecosystem,
            status: EcosystemStatus::Unsupported,
            package_count: 0,
            entry_point_count: 0,
            scanned_file_count: count,
            mounted_file_count: 0,
            unclaimed_file_count: count,
            verdict: "no reachability model; these files were counted, not audited",
            blind_spots: Vec::new(),
            note: Some(format!(
                "{count} {ecosystem} source file(s) are present and were not audited — \
                 this report cannot say whether any of them is unreachable"
            )),
            excluded_globs: Vec::new(),
            unmounted: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `contents` to `root/relative`, creating parents.
    pub(super) fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        std::fs::write(path, contents).expect("write fixture file");
    }

    pub(super) fn project(root: &Path) -> ProjectFiles {
        ProjectFiles::collect(root).expect("walk")
    }

    fn ecosystem<'a>(audit: &'a ProjectAudit, name: &str) -> &'a EcosystemAudit {
        audit
            .ecosystems
            .iter()
            .find(|entry| entry.ecosystem == name)
            .expect("ecosystem section")
    }

    /// The audit run against the repository that owns it.
    ///
    /// Ignored by default because it walks and parses the entire working tree,
    /// which is a whole-repo cost no unit-test lane should pay on every run.
    /// It is kept as a test rather than a script because it is the only check
    /// that exercises the walk at real scale, over real cargo and npm
    /// manifests, and because "our own tree is clean" is a claim that should
    /// break loudly when it stops being true:
    ///
    /// ```text
    /// cargo test --lib unmounted_files -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "walks the entire working tree; run explicitly for a dogfooding pass"]
    fn this_repository_has_no_unmounted_rust_files() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let audit = audit_project(root).expect("audit");
        for ecosystem in &audit.ecosystems {
            println!(
                "[{}] {} · packages={} entries={} scanned={} mounted={} unclaimed={} findings={}",
                ecosystem.ecosystem,
                ecosystem.status.as_str(),
                ecosystem.package_count,
                ecosystem.entry_point_count,
                ecosystem.scanned_file_count,
                ecosystem.mounted_file_count,
                ecosystem.unclaimed_file_count,
                ecosystem.unmounted.len(),
            );
            for entry in &ecosystem.unmounted {
                println!("    {} (package {})", entry.file, entry.package);
            }
        }
        let rust = ecosystem(&audit, "rust");
        let unmounted: Vec<&str> = rust
            .unmounted
            .iter()
            .map(|entry| entry.file.as_str())
            .collect();
        for required in [
            "src/sessions/claude_observation_benchmark.rs",
            "src/sessions/claude_observation_benchmark/artifact.rs",
            "src/sessions/ingest_tests.rs",
            "src/sessions/workflow_ingest_tests.rs",
            "src/profile_backup.rs",
            "src/profile_backup/error.rs",
        ] {
            assert!(
                !unmounted.contains(&required),
                "{required} must be reachable via #[path] from its declaring file; unmounted={unmounted:?}"
            );
        }
        // These three are real orphans, not auditor false positives:
        // a leftover sibling after tests were inlined, and two fixture
        // corpora that cargo-shear's ignored-paths list does not cover.
        // Pinning the exact set keeps a new false positive loud.
        assert_eq!(
            unmounted,
            [
                "crates/tracedecay-agent-hosts/src/agents/host_component_registration/tests.rs",
                "tests/fixtures/search_quality/incremental/time-after.rs",
                "tests/fixtures/storage_runtime/source_ast.rs",
            ]
        );
    }

    #[test]
    fn path_normalization_resolves_parent_traversal() {
        assert_eq!(
            normalized(Path::new("/a/b/../c/./d.rs")),
            PathBuf::from("/a/c/d.rs")
        );
    }

    /// A language with no reachability model is named with its file count, so
    /// "no findings" can never be mistaken for "nothing was looked at".
    #[test]
    fn unmodelled_languages_are_reported_as_unsupported_not_silence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "service/main.go", "package main\n");
        write(root, "tools/build.py", "print('x')\n");

        let audit = audit_project(root).expect("audit");
        let go = ecosystem(&audit, "go");
        assert_eq!(go.status.as_str(), "unsupported");
        assert_eq!(go.scanned_file_count, 1);
        assert!(go.note.is_some());
        assert_eq!(ecosystem(&audit, "python").scanned_file_count, 1);
        // The modelled ecosystems still answer, and answer "absent".
        assert_eq!(ecosystem(&audit, "rust").status.as_str(), "not_present");
        assert_eq!(
            ecosystem(&audit, "typescript").status.as_str(),
            "not_present"
        );
    }

    /// Both modelled ecosystems report in one pass over a polyglot repository.
    #[test]
    fn a_polyglot_project_reports_one_section_per_ecosystem() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
        write(root, "src/lib.rs", "");
        write(root, "src/rust_orphan.rs", "");
        write(
            root,
            "web/package.json",
            "{\"name\":\"web\",\"main\":\"src/index.js\"}",
        );
        write(root, "web/src/index.js", "export const ok = 1;\n");
        write(root, "web/src/ts_orphan.js", "export const nope = 1;\n");

        let audit = audit_project(root).expect("audit");
        let rust = ecosystem(&audit, "rust");
        assert_eq!(rust.status.as_str(), "audited");
        assert_eq!(rust.unmounted.len(), 1);
        assert_eq!(rust.unmounted[0].file, "src/rust_orphan.rs");
        let typescript = ecosystem(&audit, "typescript");
        assert_eq!(typescript.status.as_str(), "audited");
        assert_eq!(typescript.unmounted.len(), 1);
        assert_eq!(typescript.unmounted[0].file, "web/src/ts_orphan.js");
    }
}
