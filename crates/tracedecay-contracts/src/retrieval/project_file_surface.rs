//! Canonical CLI/MCP wire contracts for the project file inspections the
//! project's graph-tool owner answers: the indexed file listing and dotted-key
//! config file queries.
//!
//! Presentation-only transport keys such as `format` are removed before these
//! request bodies are decoded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How `tracedecay_files` lays out its listing.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesLayoutV1 {
    /// One file per line.
    Flat,
    /// Grouped by directory.
    #[default]
    Grouped,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesSurfaceRequestV1 {
    /// Filter to files under this directory path.
    pub path: Option<String>,
    /// Filter files matching this glob pattern (e.g. '**/*.rs').
    pub pattern: Option<String>,
    /// File listing layout: flat (one per line) or grouped by directory
    /// (default: grouped).
    pub layout: Option<FilesLayoutV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IndexedFileV1 {
    pub path: String,
    pub symbols: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesResultV1 {
    pub count: usize,
    pub layout: FilesLayoutV1,
    pub files: Vec<IndexedFileV1>,
}

/// Exactly one of `path` or `glob` names the files to query.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSurfaceRequestV1 {
    /// Dot-separated key path (e.g. 'package.version',
    /// 'dependencies.tokio.version').
    pub key: String,
    /// Project-relative path to a single config file (e.g. 'Cargo.toml').
    /// Mutually exclusive with 'glob'.
    pub path: Option<String>,
    /// Glob pattern to match multiple config files (e.g. '**/Cargo.toml').
    /// Mutually exclusive with 'path'.
    pub glob: Option<String>,
}

/// One queried config file: the file's parse error, the key's absence, or the
/// key's value and defining line.
///
/// The variants are distinguished by their exact field sets, so each one
/// denies unknown fields and the most specific shapes are tried first.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ConfigMatchV1 {
    ParseError(ConfigParseErrorV1),
    Missing(ConfigKeyMissingV1),
    Found(ConfigKeyFoundV1),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigParseErrorV1 {
    pub file: String,
    pub error: String,
}

/// `value` is always null and `found` always false; both stay on the wire
/// so a missing key reads the same as it always has.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigKeyMissingV1 {
    pub file: String,
    pub key: String,
    pub value: (),
    pub found: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigKeyFoundV1 {
    pub file: String,
    pub key: String,
    pub value: Value,
    /// The first line whose text starts with the key's leaf segment.
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigResultV1 {
    /// Files where the key was found or that failed to parse.
    pub match_count: usize,
    pub matches: Vec<ConfigMatchV1>,
}
