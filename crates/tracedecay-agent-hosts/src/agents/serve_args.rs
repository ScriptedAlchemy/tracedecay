//! The one authority for the argv a TraceDecay-written MCP registration
//! launches after the binary, so every host spells the selected catalog
//! profile the same way and every probe compares against the same bytes.

use std::collections::BTreeMap;

use serde_json::Value;
use tracedecay_contracts::catalog_composition::{CatalogProfileName, CatalogProfileNameError};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::CatalogSnapshotV1;

const SERVE: &str = "serve";
const QUOTED_SERVE: &str = "\"serve\"";
const DEFAULT_PROFILE: &str = "default";

/// The launch args after the binary: `serve`, then the catalog profile flag
/// when the host selected a non-default profile.
///
/// Never holds the `default` profile, so a default install writes exactly
/// `["serve"]`, byte-identical to earlier releases, and existing receipts and
/// digests stay current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServeArgs {
    catalog_profile: Option<CatalogProfileName>,
}

impl From<CatalogProfileName> for ServeArgs {
    fn from(name: CatalogProfileName) -> Self {
        let catalog_profile = (name.name() != DEFAULT_PROFILE).then_some(name);
        Self { catalog_profile }
    }
}

impl ServeArgs {
    /// Parses one agent's saved short name against the catalog's own MCP
    /// profiles. A name the catalog no longer has is an error naming the reset
    /// command, never a silent default.
    pub fn parse_saved(
        agent: &str,
        name: &str,
        catalog: &CatalogSnapshotV1,
    ) -> std::result::Result<Self, HostCatalogProfileError> {
        CatalogProfileName::parse(catalog, name)
            .map(Self::from)
            .map_err(|source| HostCatalogProfileError {
                agent: agent.to_owned(),
                source,
            })
    }

    pub fn catalog_profile(&self) -> Option<&CatalogProfileName> {
        self.catalog_profile.as_ref()
    }

    /// `serve [--catalog-profile=<name>] <extra...>`. The flag is one `=`
    /// token so a single command line, a TOML array, and an argv splice never
    /// split it. `extra` carries host-owned tails such as Codex's `--path .`.
    pub fn to_vec(&self, extra: &[&str]) -> Vec<String> {
        let mut args = vec![SERVE.to_owned()];
        if let Some(profile) = &self.catalog_profile {
            args.push(format!("--catalog-profile={}", profile.name()));
        }
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        args
    }

    pub fn json(&self, extra: &[&str]) -> Value {
        Value::from(self.to_vec(extra))
    }

    /// `[bin, serve, ...]` for hosts whose `command` is the whole argv.
    pub fn command_json(&self, bin: &str) -> Value {
        let mut argv = vec![bin.to_owned()];
        argv.extend(self.to_vec(&[]));
        Value::from(argv)
    }

    /// `bin serve ...` for hosts that store one command line.
    pub fn command_line(&self, bin: &str) -> String {
        let mut line = bin.to_owned();
        for arg in self.to_vec(&[]) {
            line.push(' ');
            line.push_str(&arg);
        }
        line
    }

    /// True only when `observed` is exactly what this choice writes, so a
    /// registration carrying another profile, a stale flag, or a hand edit
    /// reads as needing repair.
    pub fn matches<S: AsRef<str>>(&self, observed: &[S], extra: &[&str]) -> bool {
        let expected = self.to_vec(extra);
        observed.len() == expected.len()
            && observed
                .iter()
                .zip(&expected)
                .all(|(observed, expected)| observed.as_ref() == expected)
    }

    pub fn matches_json(&self, observed: Option<&Value>, extra: &[&str]) -> bool {
        observed == Some(&self.json(extra))
    }

    /// Compares a whole-argv `command` after its binary element.
    pub fn matches_command_json(&self, observed: Option<&Value>) -> bool {
        observed
            .and_then(Value::as_array)
            .and_then(|argv| argv.split_first())
            .and_then(|(_, args)| args.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
            .is_some_and(|args| self.matches(&args, &[]))
    }

    /// Replaces a template's literal `"serve"` element with the launch args,
    /// keeping the template's own tail (Claude's `--full-tool-list`).
    pub(crate) fn splice(&self, args: &mut Vec<Value>) -> Result<()> {
        let at = args
            .iter()
            .position(|arg| arg.as_str() == Some(SERVE))
            .ok_or_else(|| TraceDecayError::Config {
                message: "MCP server args do not launch `serve`".to_owned(),
            })?;
        args.splice(at..=at, self.to_vec(&[]).into_iter().map(Value::from));
        Ok(())
    }

    /// Replaces each quoted `"serve"` token of a compiled JSON or TypeScript
    /// template with the quoted launch args, valid inside either array form.
    pub(crate) fn render_quoted(&self, body: &str) -> String {
        if self.catalog_profile.is_none() {
            return body.to_owned();
        }
        let quoted = self
            .to_vec(&[])
            .iter()
            .map(|arg| Value::from(arg.as_str()).to_string())
            .collect::<Vec<_>>()
            .join(",");
        body.replace(QUOTED_SERVE, &quoted)
    }
}

/// Every agent's saved catalog profile, parsed once at the config boundary.
/// An agent with no entry launches the default profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostCatalogProfiles(BTreeMap<String, ServeArgs>);

#[derive(Debug, thiserror::Error)]
#[error(
    "saved catalog profile for agent {agent:?} is invalid ({source}); \
     run `tracedecay install --agent {agent} --catalog-profile default`"
)]
pub struct HostCatalogProfileError {
    agent: String,
    #[source]
    source: CatalogProfileNameError,
}

impl HostCatalogProfiles {
    /// Parses every saved entry with [`ServeArgs::parse_saved`].
    pub fn parse(
        saved: &BTreeMap<String, String>,
        catalog: &CatalogSnapshotV1,
    ) -> std::result::Result<Self, HostCatalogProfileError> {
        saved
            .iter()
            .map(|(agent, name)| {
                ServeArgs::parse_saved(agent, name, catalog).map(|args| (agent.clone(), args))
            })
            .collect::<std::result::Result<_, _>>()
            .map(Self)
    }

    /// One agent's choice, while a lifecycle operation has not saved it yet.
    pub fn only(agent_id: &str, serve_args: ServeArgs) -> Self {
        Self(BTreeMap::from([(agent_id.to_owned(), serve_args)]))
    }

    pub fn serve_args(&self, agent_id: &str) -> ServeArgs {
        self.0.get(agent_id).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tracedecay_contracts::catalog_composition::build_application_binding_snapshot;

    fn profile(name: &str) -> ServeArgs {
        let catalog = build_application_binding_snapshot().expect("catalog");
        ServeArgs::from(CatalogProfileName::parse(&catalog, name).expect("profile"))
    }

    #[test]
    fn default_writes_the_historical_bytes_and_compact_one_flag_token() {
        assert_eq!(profile("default"), ServeArgs::default());
        assert_eq!(profile("default").json(&[]), json!(["serve"]));
        let compact = profile("compact");
        assert_eq!(
            compact.json(&["--path", "."]),
            json!(["serve", "--catalog-profile=compact", "--path", "."])
        );
        assert_eq!(
            compact.command_line("/bin/td"),
            "/bin/td serve --catalog-profile=compact"
        );
        let mut args = vec![json!("serve"), json!("--full-tool-list")];
        compact.splice(&mut args).expect("splice");
        assert_eq!(
            args,
            [
                json!("serve"),
                json!("--catalog-profile=compact"),
                json!("--full-tool-list")
            ]
        );
        assert_eq!(
            compact.render_quoted(r#"{"args":["serve"]}"#),
            r#"{"args":["serve","--catalog-profile=compact"]}"#
        );
    }

    #[test]
    fn only_the_exact_written_args_match() {
        let compact = profile("compact");
        let default = ServeArgs::default();
        let written = compact.json(&[]);
        assert!(compact.matches_json(Some(&written), &[]));
        assert!(!default.matches_json(Some(&written), &[]));
        assert!(!compact.matches_json(Some(&json!(["serve"])), &[]));
        assert!(compact.matches_command_json(Some(&compact.command_json("/x"))));
        assert!(!default.matches_command_json(Some(&compact.command_json("/x"))));
        assert!(compact.matches(&["serve", "--catalog-profile=compact"], &[]));
        assert!(!compact.matches(&["serve"], &[]));
    }

    #[test]
    fn a_saved_name_the_catalog_lacks_names_the_reset_command() {
        let catalog = build_application_binding_snapshot().expect("catalog");
        let saved = BTreeMap::from([("cursor".to_owned(), "tiny".to_owned())]);
        let error = HostCatalogProfiles::parse(&saved, &catalog).expect_err("unknown");
        assert!(
            error
                .to_string()
                .contains("tracedecay install --agent cursor --catalog-profile default"),
            "{error}"
        );
        let saved = BTreeMap::from([("cursor".to_owned(), "compact".to_owned())]);
        let parsed = HostCatalogProfiles::parse(&saved, &catalog).expect("compact");
        assert_eq!(parsed.serve_args("cursor"), profile("compact"));
        assert_eq!(parsed.serve_args("zed"), ServeArgs::default());
    }
}
