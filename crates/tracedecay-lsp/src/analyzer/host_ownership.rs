//! Host-declared analyzer ownership consumed by the analyzer broker.
//!
//! Plan 27 (`docs/plans/tracedecay-v2/27-cross-host-agent-plugin-bundles.md`)
//! requires that "`OpenCode` conformance starts the `TraceDecay` custom LSP with an
//! existing language analyzer present and proves exactly one analyzer owns that
//! language before, during, and after install, repair, rollback, and uninstall
//! while `TraceDecay` findings still project", and Plan 35
//! (`35-daemon-lsp-gateway-and-universal-diagnostics.md`) requires `OpenCode` to
//! retain "exactly one analyzer per language through install, repair, rollback,
//! and uninstall".
//!
//! The `OpenCode` installer already records that contract in the host's own
//! configuration — `lsp.tracedecay.initialization.tracedecay` carries
//! `duplicateAnalyzerAvoidance` plus an `analyzerOwnership.retainedByExtension`
//! map naming the analyzer the host already runs for each extension. This
//! module is where that declaration stops being inert configuration: the broker
//! reads it and refuses to start a second analyzer for a retained language.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Project-level `OpenCode` configuration file the installer writes.
///
/// `crates/tracedecay-agent-hosts/src/agents/opencode.rs` installs the same
/// registration into `<project>/opencode.json` and into the home-level
/// `~/.config/opencode/opencode.json`. The broker is project-scoped, so it
/// reads the project file directly and takes the home-level declaration through
/// [`super::broker::DiagnosticBroker::adopt_host_analyzer_ownership`].
pub const OPENCODE_PROJECT_CONFIG_FILE: &str = "opencode.json";

/// One host's declaration of which analyzers it already owns.
///
/// An empty value (the default) means no host declared ownership, which is the
/// state every non-`OpenCode` host and every pre-install project is in. Ownership
/// is only enforced when the host both asked for duplicate-analyzer avoidance
/// and named at least one retained analyzer: a host that asks for avoidance but
/// retains nothing has no competing analyzer to avoid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostAnalyzerOwnership {
    duplicate_analyzer_avoidance: bool,
    retained_by_extension: BTreeMap<String, BTreeSet<String>>,
}

impl HostAnalyzerOwnership {
    /// Builds ownership from the `initialization.tracedecay` object `OpenCode`
    /// hands the `TraceDecay` LSP registration.
    pub fn from_tracedecay_initialization(tracedecay: &Value) -> Self {
        let duplicate_analyzer_avoidance = tracedecay
            .get("duplicateAnalyzerAvoidance")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut retained_by_extension: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        if let Some(retained) = tracedecay
            .get("analyzerOwnership")
            .and_then(|ownership| ownership.get("retainedByExtension"))
            .and_then(Value::as_object)
        {
            for (extension, owners) in retained {
                let Some(extension) = normalized_extension(extension) else {
                    continue;
                };
                let owners = owners
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|owner| !owner.is_empty())
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                if owners.is_empty() {
                    continue;
                }
                retained_by_extension
                    .entry(extension)
                    .or_default()
                    .extend(owners);
            }
        }
        Self {
            duplicate_analyzer_avoidance,
            retained_by_extension,
        }
    }

    /// Builds ownership from a whole `OpenCode` configuration document.
    pub fn from_opencode_config(config: &Value) -> Self {
        config
            .get("lsp")
            .and_then(|lsp| lsp.get("tracedecay"))
            .and_then(|registration| registration.get("initialization"))
            .and_then(|initialization| initialization.get("tracedecay"))
            .map(Self::from_tracedecay_initialization)
            .unwrap_or_default()
    }

    /// Reads the project-level `OpenCode` configuration, if the project has one.
    ///
    /// A missing, unreadable, or non-JSON configuration yields no declared
    /// ownership: this file belongs to the host, and `TraceDecay` cannot invent an
    /// ownership claim the host never made. It never *relaxes* a claim either —
    /// a claim only ever comes from a parsed declaration.
    pub fn from_opencode_project_root(project_root: &Path) -> Self {
        Self::from_opencode_config_file(&project_root.join(OPENCODE_PROJECT_CONFIG_FILE))
    }

    /// Reads one `OpenCode` configuration file, project- or home-level.
    ///
    /// The same refusal-to-invent rule as
    /// [`Self::from_opencode_project_root`] applies: a missing, unreadable,
    /// or non-JSON file declares nothing.
    pub fn from_opencode_config_file(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        let Ok(config) = serde_json::from_slice::<Value>(&bytes) else {
            return Self::default();
        };
        Self::from_opencode_config(&config)
    }

    /// Reads the home-level `OpenCode` configuration of the running process
    /// user (`$XDG_CONFIG_HOME/opencode/opencode.json`, else
    /// `~/.config/opencode/opencode.json`).
    ///
    /// The installer writes the same registration to both the project and the
    /// home level; a host that was only registered at the home level still
    /// declared its analyzer ownership, so the broker has to honor it. A
    /// process without a resolvable home declares nothing.
    pub fn from_opencode_process_home() -> Self {
        match process_home_opencode_config_path() {
            Some(path) => Self::from_opencode_config_file(&path),
            None => Self::default(),
        }
    }

    /// Combines two host declarations into the ownership actually in force.
    ///
    /// Each side contributes only when it is individually engaged: a
    /// declaration that retained analyzers but never asked for
    /// duplicate-analyzer avoidance stays unenforced even when the other
    /// level asked for avoidance over a different extension set.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut merged = Self::default();
        for side in [self, other] {
            if !side.is_engaged() {
                continue;
            }
            merged.duplicate_analyzer_avoidance = true;
            for (extension, owners) in &side.retained_by_extension {
                merged
                    .retained_by_extension
                    .entry(extension.clone())
                    .or_default()
                    .extend(owners.iter().cloned());
            }
        }
        merged
    }

    /// True when the host asked for duplicate-analyzer avoidance *and* named at
    /// least one analyzer it already owns.
    pub fn is_engaged(&self) -> bool {
        self.duplicate_analyzer_avoidance && !self.retained_by_extension.is_empty()
    }

    /// The analyzer the host already runs for `extension`, if any.
    pub fn retained_owner_for_extension(&self, extension: &str) -> Option<&str> {
        if !self.is_engaged() {
            return None;
        }
        let extension = normalized_extension(extension)?;
        self.retained_by_extension
            .get(&extension)
            .and_then(|owners| owners.iter().next())
            .map(String::as_str)
    }

    /// The analyzer the host already runs for any of `extensions`.
    pub fn retained_owner_for_extensions<'a, I>(&self, extensions: I) -> Option<&str>
    where
        I: IntoIterator<Item = &'a str>,
    {
        extensions
            .into_iter()
            .find_map(|extension| self.retained_owner_for_extension(extension))
    }
}

/// The home-level `OpenCode` configuration path under an explicit config root.
///
/// `$XDG_CONFIG_HOME` wins only when it is absolute — a relative value does
/// not name a usable config root and `OpenCode` itself falls back to
/// `~/.config`. This mirrors the resolution the `OpenCode` installer uses when
/// it writes the registration, so the broker reads the same file the
/// installer wrote.
pub fn opencode_home_config_path(home: &Path, xdg_config_home: Option<&OsStr>) -> PathBuf {
    xdg_config_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("opencode")
        .join("opencode.json")
}

/// Resolves the running process user's home-level `OpenCode` configuration
/// path from the ambient environment, or `None` without a resolvable home.
fn process_home_opencode_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(opencode_home_config_path(
        &home,
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
    ))
}

/// Normalizes `".RS"`, `"rs"`, and `".rs"` to `"rs"`.
///
/// The `OpenCode` registration writes dotted extensions (`".rs"`) while the
/// analyzer adapters carry bare ones (`"rs"`), so a raw string compare would
/// silently never match and the whole ownership contract would read as "nothing
/// retained".
fn normalized_extension(extension: &str) -> Option<String> {
    let trimmed = extension.trim().trim_start_matches('.');
    (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opencode_config(retained: Value) -> Value {
        json!({
            "lsp": {
                "tracedecay": {
                    "command": ["tracedecay", "lsp", "bridge", "--stdio"],
                    "initialization": {
                        "tracedecay": {
                            "brokerUpstream": false,
                            "duplicateAnalyzerAvoidance": true,
                            "analyzerOwnership": {
                                "mode": "projection_only",
                                "retainedByExtension": retained
                            }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn dotted_registration_extensions_match_bare_adapter_extensions() {
        let ownership = HostAnalyzerOwnership::from_opencode_config(&opencode_config(json!({
            ".rs": ["rust-analyzer"]
        })));

        assert!(ownership.is_engaged());
        assert_eq!(
            ownership.retained_owner_for_extension("rs"),
            Some("rust-analyzer"),
            "adapters carry bare extensions; the registration writes dotted ones"
        );
        assert_eq!(ownership.retained_owner_for_extension("ts"), None);
    }

    #[test]
    fn avoidance_without_a_retained_analyzer_claims_nothing() {
        let ownership = HostAnalyzerOwnership::from_opencode_config(&opencode_config(json!({})));

        assert!(
            !ownership.is_engaged(),
            "there is no competing analyzer to avoid when the host retains none"
        );
        assert_eq!(ownership.retained_owner_for_extension("rs"), None);
    }

    #[test]
    fn retained_analyzers_without_avoidance_are_not_enforced() {
        let mut config = opencode_config(json!({ ".rs": ["rust-analyzer"] }));
        config["lsp"]["tracedecay"]["initialization"]["tracedecay"]["duplicateAnalyzerAvoidance"] =
            json!(false);

        let ownership = HostAnalyzerOwnership::from_opencode_config(&config);

        assert!(!ownership.is_engaged());
        assert_eq!(ownership.retained_owner_for_extension("rs"), None);
    }

    #[test]
    fn a_project_without_an_opencode_config_declares_no_ownership() {
        let project = tempfile::tempdir().expect("project root");

        let ownership = HostAnalyzerOwnership::from_opencode_project_root(project.path());

        assert_eq!(ownership, HostAnalyzerOwnership::default());
        assert!(!ownership.is_engaged());
    }

    #[test]
    fn a_project_opencode_config_is_read_from_disk() {
        let project = tempfile::tempdir().expect("project root");
        std::fs::write(
            project.path().join(OPENCODE_PROJECT_CONFIG_FILE),
            serde_json::to_vec_pretty(&opencode_config(json!({ ".ts": ["typescript"] }))).unwrap(),
        )
        .expect("write project opencode.json");

        let ownership = HostAnalyzerOwnership::from_opencode_project_root(project.path());

        assert_eq!(
            ownership.retained_owner_for_extensions(["tsx", "ts"]),
            Some("typescript")
        );
    }

    #[test]
    fn an_unparseable_config_never_invents_an_ownership_claim() {
        let project = tempfile::tempdir().expect("project root");
        std::fs::write(
            project.path().join(OPENCODE_PROJECT_CONFIG_FILE),
            b"{ not json",
        )
        .expect("write invalid project opencode.json");

        assert!(!HostAnalyzerOwnership::from_opencode_project_root(project.path()).is_engaged());
    }

    #[test]
    fn the_home_config_path_prefers_an_absolute_xdg_config_home() {
        let home = Path::new("/isolated/home");

        assert_eq!(
            opencode_home_config_path(home, None),
            Path::new("/isolated/home/.config/opencode/opencode.json")
        );
        assert_eq!(
            opencode_home_config_path(home, Some(OsStr::new("/xdg/config"))),
            Path::new("/xdg/config/opencode/opencode.json")
        );
        assert_eq!(
            opencode_home_config_path(home, Some(OsStr::new("relative/config"))),
            Path::new("/isolated/home/.config/opencode/opencode.json"),
            "a relative $XDG_CONFIG_HOME does not name a usable config root"
        );
    }

    #[test]
    fn a_home_level_config_is_read_through_the_shared_file_reader() {
        let config_root = tempfile::tempdir().expect("config root");
        let opencode_dir = config_root.path().join("opencode");
        std::fs::create_dir_all(&opencode_dir).expect("opencode config dir");
        std::fs::write(
            opencode_dir.join("opencode.json"),
            serde_json::to_vec_pretty(&opencode_config(json!({ ".rs": ["rust-analyzer"] })))
                .unwrap(),
        )
        .expect("write home opencode.json");

        let path = opencode_home_config_path(
            Path::new("/nonexistent/home"),
            Some(config_root.path().as_os_str()),
        );
        let ownership = HostAnalyzerOwnership::from_opencode_config_file(&path);

        assert_eq!(
            ownership.retained_owner_for_extension("rs"),
            Some("rust-analyzer")
        );
    }

    #[test]
    fn a_union_keeps_only_individually_engaged_declarations() {
        let engaged_home = HostAnalyzerOwnership::from_opencode_config(&opencode_config(json!({
            ".ts": ["typescript"]
        })));
        let mut unengaged_project_config = opencode_config(json!({ ".rs": ["rust-analyzer"] }));
        unengaged_project_config["lsp"]["tracedecay"]["initialization"]["tracedecay"]["duplicateAnalyzerAvoidance"] =
            json!(false);
        let unengaged_project =
            HostAnalyzerOwnership::from_opencode_config(&unengaged_project_config);

        let merged = unengaged_project.union(&engaged_home);

        assert!(merged.is_engaged());
        assert_eq!(
            merged.retained_owner_for_extension("ts"),
            Some("typescript")
        );
        assert_eq!(
            merged.retained_owner_for_extension("rs"),
            None,
            "a level that never asked for avoidance keeps its retained map unenforced"
        );
    }

    #[test]
    fn a_union_of_two_engaged_declarations_covers_both_extension_sets() {
        let project = HostAnalyzerOwnership::from_opencode_config(&opencode_config(json!({
            ".rs": ["rust-analyzer"]
        })));
        let home = HostAnalyzerOwnership::from_opencode_config(&opencode_config(json!({
            ".ts": ["typescript"]
        })));

        let merged = project.union(&home);

        assert_eq!(
            merged.retained_owner_for_extension("rs"),
            Some("rust-analyzer")
        );
        assert_eq!(
            merged.retained_owner_for_extension("ts"),
            Some("typescript")
        );
    }

    #[test]
    fn a_union_of_unengaged_declarations_stays_unengaged() {
        let merged = HostAnalyzerOwnership::default().union(&HostAnalyzerOwnership::default());

        assert!(!merged.is_engaged());
        assert_eq!(merged, HostAnalyzerOwnership::default());
    }
}
