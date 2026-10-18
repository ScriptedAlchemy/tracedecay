/// Why an unavailable host sweep could not read its source.
///
/// Persisted through the closed `host-coverage://` code table in
/// `HostProviderCoverage::file_id`, never as bits beside the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCoverageReason {
    DatabaseMissing,
    DatabaseNotAFile,
    DatabaseUnreadable,
    SourceIdentityUnavailable,
}
