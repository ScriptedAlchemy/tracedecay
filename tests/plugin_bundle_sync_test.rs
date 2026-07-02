//! Cross-bundle sync enforcement for the shipped plugin source bundles.
//!
//! `cursor-plugin/` and `codex-plugin/` (and any future ecosystem bundle,
//! e.g. `claude-plugin/`) must not silently drift: every content unit is
//! either present and byte-identical in all bundles, or covered by a
//! declarative exception below that documents why it diverges or is absent.
//! Adding a bundle means adding one `Bundle` row plus manifest entries — the
//! assertions themselves are bundle-count agnostic.
//!
//! Division of labour with existing tests (do not duplicate them here):
//! - `tests/agent_suite/plugin_skill_contract_test.rs` — per-host frontmatter
//!   schema and skill-creator design budgets for each bundle in isolation.
//! - `src/agents/cursor.rs` unit tests
//!   (`embedded_file_list_covers_the_whole_source_bundle`) and
//!   `src/agents/codex.rs` unit tests
//!   (`codex_embedded_file_list_covers_the_whole_source_bundle`) — the
//!   private `EMBEDDED_PLUGIN_FILES` / `CODEX_EMBEDDED_PLUGIN_FILES`
//!   registries must cover exactly the on-disk bundle trees. Because those
//!   pin registry == disk, the disk-level sync enforced here transitively
//!   keeps the embedded registries in sync too.
//! - `src/agents/codex.rs` `codex_skills_match_the_cursor_source_for_parity`
//!   — the original two-bundle skill parity check with its own allowlists.
//!   The skill exceptions below mirror those allowlists; if the two tables
//!   drift apart, one of the two tests fails and points at the other.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tracedecay::hooks::CURSOR_PLUGIN_SKILLS;

/// One shipped plugin source bundle, rooted at the repo top level.
struct Bundle {
    name: &'static str,
    root: &'static str,
}

/// Every ecosystem bundle shipped from this repo. A third ecosystem (e.g.
/// `claude-plugin/`) joins the sync check by adding a row here plus its
/// host-specific rows in [`TOP_LEVEL_MANIFEST`] / [`SKILL_SYNC_EXCEPTIONS`].
const BUNDLES: &[Bundle] = &[
    Bundle {
        name: "cursor",
        root: "cursor-plugin",
    },
    Bundle {
        name: "codex",
        root: "codex-plugin",
    },
];

/// The directory of shared content units governed by [`SKILL_SYNC_EXCEPTIONS`].
const SKILLS_DIR: &str = "skills";

/// Sync policy for a top-level bundle entry (file or directory).
enum TopLevelPolicy {
    /// Present in every bundle; child units must be byte-identical across
    /// bundles unless excepted in [`SKILL_SYNC_EXCEPTIONS`].
    SyncedSkills,
    /// Present in every bundle, but the content is host-specific by design.
    HostSpecific { reason: &'static str },
    /// Present only in the listed bundles.
    OnlyIn {
        bundles: &'static [&'static str],
        reason: &'static str,
    },
}

/// Declarative manifest of every top-level entry a bundle may contain.
/// [`bundle_top_level_entries_match_the_sync_manifest`] enforces exact
/// equality in both directions, so a new content category (or a stale row)
/// cannot slip in without a documented policy.
const TOP_LEVEL_MANIFEST: &[(&str, TopLevelPolicy)] = &[
    (
        ".cursor-plugin",
        TopLevelPolicy::OnlyIn {
            bundles: &["cursor"],
            reason: "Cursor requires the manifest at .cursor-plugin/plugin.json",
        },
    ),
    (
        ".codex-plugin",
        TopLevelPolicy::OnlyIn {
            bundles: &["codex"],
            reason: "Codex requires the manifest at .codex-plugin/plugin.json",
        },
    ),
    (
        "mcp.json",
        TopLevelPolicy::OnlyIn {
            bundles: &["cursor"],
            reason: "Cursor's manifest points at a root mcp.json for MCP server config",
        },
    ),
    (
        ".mcp.json",
        TopLevelPolicy::OnlyIn {
            bundles: &["codex"],
            reason: "Codex reads MCP server config from a dotted .mcp.json at the bundle root",
        },
    ),
    (
        "README.md",
        TopLevelPolicy::HostSpecific {
            reason: "install and usage instructions are written per host",
        },
    ),
    (
        "hooks",
        TopLevelPolicy::HostSpecific {
            reason: "cursor ships hook-cursor-* commands; the codex source bundle ships an \
                     empty {\"hooks\": {}} stub and the installer bakes commands at render \
                     time",
        },
    ),
    (
        "agents",
        TopLevelPolicy::OnlyIn {
            bundles: &["cursor"],
            reason: "subagent definitions are a Cursor-only plugin surface",
        },
    ),
    (
        "rules",
        TopLevelPolicy::OnlyIn {
            bundles: &["cursor"],
            reason: "always-applied rules are a Cursor-only plugin surface; Codex gets \
                     equivalent steering via session context (src/hooks.rs)",
        },
    ),
    ("skills", TopLevelPolicy::SyncedSkills),
];

/// How a skill is allowed to deviate from full cross-bundle byte parity.
enum SkillSyncRule {
    /// Shipped only in the listed bundles (must match actual presence
    /// exactly, so a stale exception fails).
    OnlyIn {
        bundles: &'static [&'static str],
        reason: &'static str,
    },
    /// Shipped in every bundle, but the SKILL.md bodies are intentionally
    /// host-specific (they must actually differ, so a healed divergence
    /// fails until the exception is removed).
    DivergentBody { reason: &'static str },
    /// Shipped in every bundle with byte-identical bodies after the YAML
    /// frontmatter; only the frontmatter may (and must) differ.
    DivergentFrontmatter { reason: &'static str },
}

/// Documented exceptions to "every skill ships in every bundle,
/// byte-identical". Skills absent from this table get the strict default.
/// These entries mirror the allowlists in `src/agents/codex.rs`
/// (`CODEX_SKILL_BODY_DIVERGENCES` / `CODEX_SKILL_FRONTMATTER_DIVERGENCES`);
/// keep the reasons in step when editing either side.
const SKILL_SYNC_EXCEPTIONS: &[(&[&str], SkillSyncRule)] = &[
    (
        &["memorize-subject", "memorizing-subject"],
        SkillSyncRule::OnlyIn {
            bundles: &["cursor"],
            reason: "explicit-invoke (disable-model-invocation: true) memory workflows; \
                     Codex has no explicit-invoke surface, and its \
                     curating-project-memory copy inlines the guardrails instead",
        },
    ),
    (
        &[
            "tracedecay-audit-safety",
            "tracedecay-check-health",
            "tracedecay-clean-dead-code",
            "tracedecay-compare-branches",
            "tracedecay-curate-memory",
            "tracedecay-draft-commit",
            "tracedecay-find-impact",
            "tracedecay-fix-build",
            "tracedecay-map-architecture",
            "tracedecay-port-code",
            "tracedecay-recall-memory",
            "tracedecay-review-diff",
            "tracedecay-test-changes",
        ],
        SkillSyncRule::OnlyIn {
            bundles: &["cursor"],
            reason: "Cursor-only slash dispatchers (disable-model-invocation: true) that \
                     hand off to the shared workflow skills; Codex auto-discovers the \
                     workflow skills directly",
        },
    ),
    (
        &["curating-project-memory"],
        SkillSyncRule::DivergentBody {
            reason: "the Cursor source hands the add-a-subject flow to the \
                     memorizing-subject skill, which Codex does not ship; the Codex copy \
                     inlines that flow's guardrails instead of pointing at an absent skill",
        },
    ),
    (
        &["running-impacted-tests"],
        SkillSyncRule::DivergentFrontmatter {
            reason: "Cursor keeps `paths` frontmatter so the host can path-scope the \
                     skill; Codex must omit that key to satisfy the Codex skill-creator \
                     quick_validate.py schema",
        },
    ),
];

#[test]
fn bundle_top_level_entries_match_the_sync_manifest() {
    assert_only_in_lists_name_real_bundles();
    for bundle in BUNDLES {
        let actual = sorted_dir_entry_names(&repo_path(bundle.root));
        let mut expected: Vec<String> = TOP_LEVEL_MANIFEST
            .iter()
            .filter(|(_, policy)| policy_applies_to(policy, bundle.name))
            .map(|&(entry, _)| entry.to_string())
            .collect();
        expected.sort();
        assert_eq!(
            actual, expected,
            "{}/ top-level entries drifted from TOP_LEVEL_MANIFEST in \
             tests/plugin_bundle_sync_test.rs; declare new content units with a sync \
             policy (or remove stale manifest rows)",
            bundle.root
        );
    }
}

#[test]
fn skills_are_synced_across_bundles_or_declared_exceptions() {
    let presence = skill_presence_by_bundle();
    let exceptions = skill_exception_index();

    for &skill in exceptions.keys() {
        assert!(
            presence.contains_key(skill),
            "SKILL_SYNC_EXCEPTIONS names `{skill}`, which no bundle ships; remove the \
             stale exception"
        );
    }

    let every_bundle: BTreeSet<&'static str> = BUNDLES.iter().map(|bundle| bundle.name).collect();
    for (skill, shipped_in) in &presence {
        match exceptions.get(skill.as_str()) {
            Some(SkillSyncRule::OnlyIn { bundles, reason }) => {
                let declared: BTreeSet<&'static str> = bundles.iter().copied().collect();
                assert_eq!(
                    shipped_in, &declared,
                    "skill `{skill}` is declared OnlyIn {declared:?} ({reason}) but ships \
                     in {shipped_in:?}; fix the bundles or the exception"
                );
            }
            Some(SkillSyncRule::DivergentBody { .. })
            | Some(SkillSyncRule::DivergentFrontmatter { .. })
            | None => {
                assert_eq!(
                    shipped_in, &every_bundle,
                    "skill `{skill}` must ship in every bundle (or be declared OnlyIn in \
                     SKILL_SYNC_EXCEPTIONS with a reason) but ships only in {shipped_in:?}"
                );
                assert_skill_content_synced(skill, exceptions.get(skill.as_str()));
            }
        }
    }
}

/// The cross-bundle shared skill set must equal the runtime skill index the
/// hooks advertise (`hooks::CURSOR_PLUGIN_SKILLS`), tying this manifest to
/// the session-context steering and to the codex.rs parity unit tests. If a
/// future bundle intentionally ships a subset, its missing skills become
/// `OnlyIn` exceptions and this expectation must be revisited alongside them.
#[test]
fn skills_shared_by_every_bundle_match_the_runtime_skill_index() {
    let bundle_count = BUNDLES.len();
    let shared: Vec<String> = skill_presence_by_bundle()
        .into_iter()
        .filter(|(_, shipped_in)| shipped_in.len() == bundle_count)
        .map(|(skill, _)| skill)
        .collect();
    let mut expected: Vec<String> = CURSOR_PLUGIN_SKILLS
        .iter()
        .map(|skill| (*skill).to_string())
        .collect();
    expected.sort();
    assert_eq!(
        shared, expected,
        "the skills shared by every bundle must be exactly \
         hooks::CURSOR_PLUGIN_SKILLS (the model-invocable workflow set)"
    );
}

/// Every exception must carry a non-empty written reason: the manifest is
/// the documentation of *why* a unit is allowed to diverge, and an empty
/// reason defeats it.
#[test]
fn every_sync_exception_documents_a_reason() {
    for (entry, policy) in TOP_LEVEL_MANIFEST {
        match policy {
            TopLevelPolicy::SyncedSkills => {}
            TopLevelPolicy::HostSpecific { reason } | TopLevelPolicy::OnlyIn { reason, .. } => {
                assert!(
                    !reason.trim().is_empty(),
                    "TOP_LEVEL_MANIFEST entry `{entry}` needs a written reason"
                );
            }
        }
    }
    for (skills, rule) in SKILL_SYNC_EXCEPTIONS {
        let reason = match rule {
            SkillSyncRule::OnlyIn { reason, .. }
            | SkillSyncRule::DivergentBody { reason }
            | SkillSyncRule::DivergentFrontmatter { reason } => reason,
        };
        assert!(
            !reason.trim().is_empty(),
            "SKILL_SYNC_EXCEPTIONS entry {skills:?} needs a written reason"
        );
    }
}

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn policy_applies_to(policy: &TopLevelPolicy, bundle: &str) -> bool {
    match policy {
        TopLevelPolicy::SyncedSkills | TopLevelPolicy::HostSpecific { .. } => true,
        TopLevelPolicy::OnlyIn { bundles, .. } => bundles.contains(&bundle),
    }
}

fn assert_only_in_lists_name_real_bundles() {
    let known: BTreeSet<&'static str> = BUNDLES.iter().map(|bundle| bundle.name).collect();
    let check = |context: String, bundles: &[&str]| {
        for name in bundles {
            assert!(
                known.contains(name),
                "{context} names unknown bundle `{name}`; known bundles are {known:?}"
            );
        }
    };
    for (entry, policy) in TOP_LEVEL_MANIFEST {
        match policy {
            TopLevelPolicy::SyncedSkills | TopLevelPolicy::HostSpecific { .. } => {}
            TopLevelPolicy::OnlyIn { bundles, .. } => {
                check(format!("TOP_LEVEL_MANIFEST entry `{entry}`"), bundles);
            }
        }
    }
    for (skills, rule) in SKILL_SYNC_EXCEPTIONS {
        match rule {
            SkillSyncRule::OnlyIn { bundles, .. } => {
                check(format!("SKILL_SYNC_EXCEPTIONS entry {skills:?}"), bundles);
            }
            SkillSyncRule::DivergentBody { .. } | SkillSyncRule::DivergentFrontmatter { .. } => {}
        }
    }
}

fn sorted_dir_entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        .map(|entry| {
            entry
                .expect("read dir entry")
                .file_name()
                .to_str()
                .expect("bundle entry names should be utf-8")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// Maps each skill name to the set of bundles that ship it. Also asserts the
/// skills directories contain only skill directories (a stray file there
/// belongs to no unit and would escape the sync check).
fn skill_presence_by_bundle() -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut presence: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for bundle in BUNDLES {
        let skills_root = repo_path(bundle.root).join(SKILLS_DIR);
        for entry in std::fs::read_dir(&skills_root)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", skills_root.display()))
        {
            let path = entry.expect("read skills dir entry").path();
            assert!(
                path.is_dir(),
                "{} is a stray file; {}/{SKILLS_DIR}/ may contain only skill directories",
                path.display(),
                bundle.root
            );
            let skill = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("skill directory names should be utf-8")
                .to_string();
            presence.entry(skill).or_default().insert(bundle.name);
        }
    }
    presence
}

fn skill_exception_index() -> BTreeMap<&'static str, &'static SkillSyncRule> {
    let mut index = BTreeMap::new();
    for (skills, rule) in SKILL_SYNC_EXCEPTIONS {
        for &skill in *skills {
            assert!(
                index.insert(skill, rule).is_none(),
                "SKILL_SYNC_EXCEPTIONS lists `{skill}` more than once"
            );
        }
    }
    index
}

/// Compares one skill's directory tree across every bundle against the first
/// bundle's copy: identical file sets, and byte-identical file contents
/// except where `rule` documents an intentional SKILL.md divergence.
fn assert_skill_content_synced(skill: &str, rule: Option<&&SkillSyncRule>) {
    let reference = &BUNDLES[0];
    let reference_dir = repo_path(reference.root).join(SKILLS_DIR).join(skill);
    let reference_files = relative_files_under(&reference_dir);

    for bundle in &BUNDLES[1..] {
        let bundle_dir = repo_path(bundle.root).join(SKILLS_DIR).join(skill);
        assert_eq!(
            relative_files_under(&bundle_dir),
            reference_files,
            "skill `{skill}` ships different file sets in {} and {}",
            reference.root,
            bundle.root
        );
        for relative in &reference_files {
            let reference_file = reference_dir.join(relative);
            let bundle_file = bundle_dir.join(relative);
            if relative == Path::new("SKILL.md") {
                assert_skill_md_synced(skill, rule, &reference_file, &bundle_file);
            } else {
                assert!(
                    read_bytes(&bundle_file) == read_bytes(&reference_file),
                    "{} must be byte-identical to {}",
                    bundle_file.display(),
                    reference_file.display()
                );
            }
        }
    }
}

fn assert_skill_md_synced(
    skill: &str,
    rule: Option<&&SkillSyncRule>,
    reference_file: &Path,
    bundle_file: &Path,
) {
    match rule {
        None => {
            assert!(
                read_bytes(bundle_file) == read_bytes(reference_file),
                "{} must be byte-identical to {} (add `{skill}` to \
                 SKILL_SYNC_EXCEPTIONS with a reason if a host-specific version is \
                 intended)",
                bundle_file.display(),
                reference_file.display()
            );
        }
        Some(SkillSyncRule::DivergentBody { .. }) => {
            assert!(
                read_bytes(bundle_file) != read_bytes(reference_file),
                "{} no longer diverges from {}; remove `{skill}` from \
                 SKILL_SYNC_EXCEPTIONS so full parity is enforced again",
                bundle_file.display(),
                reference_file.display()
            );
        }
        Some(SkillSyncRule::DivergentFrontmatter { .. }) => {
            let reference_doc = read_text(reference_file);
            let bundle_doc = read_text(bundle_file);
            let (reference_frontmatter, reference_body) =
                split_frontmatter(reference_file, &reference_doc);
            let (bundle_frontmatter, bundle_body) = split_frontmatter(bundle_file, &bundle_doc);
            assert_eq!(
                bundle_body,
                reference_body,
                "{} body (after frontmatter) must mirror {}",
                bundle_file.display(),
                reference_file.display()
            );
            assert_ne!(
                bundle_frontmatter,
                reference_frontmatter,
                "{} frontmatter no longer diverges from {}; remove `{skill}` from \
                 SKILL_SYNC_EXCEPTIONS so full parity is enforced again",
                bundle_file.display(),
                reference_file.display()
            );
        }
        Some(SkillSyncRule::OnlyIn { .. }) => {
            unreachable!("OnlyIn skills are never content-compared across bundles")
        }
    }
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

/// Splits a skill document into (frontmatter lines, body lines). Line-based
/// so CRLF checkouts compare like LF ones.
fn split_frontmatter<'doc>(path: &Path, contents: &'doc str) -> (Vec<&'doc str>, Vec<&'doc str>) {
    let mut lines = contents.lines();
    assert_eq!(
        lines.next().map(str::trim),
        Some("---"),
        "{} must open with YAML frontmatter",
        path.display()
    );
    let mut frontmatter = Vec::new();
    for line in lines.by_ref() {
        if line.trim() == "---" {
            return (frontmatter, lines.collect());
        }
        frontmatter.push(line);
    }
    panic!("{} never closes its YAML frontmatter", path.display());
}

fn relative_files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let path = entry.expect("read skill tree entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .expect("collected paths live under root")
                        .to_path_buf(),
                );
            }
        }
    }
    files.sort();
    files
}
