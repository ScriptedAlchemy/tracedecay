/// Why an unavailable host sweep could not read its source.
///
/// Persisted as its own `parse_offsets.coverage_reason` value. The coverage
/// state stays in `file_id`; the two fields are not packed together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u64)]
pub enum HostCoverageReason {
    DatabaseMissing = 1,
    DatabaseNotAFile = 2,
    DatabaseUnreadable = 3,
    SourceIdentityUnavailable = 4,
}

impl HostCoverageReason {
    pub const fn from_code(code: u64) -> Option<Self> {
        match code {
            1 => Some(Self::DatabaseMissing),
            2 => Some(Self::DatabaseNotAFile),
            3 => Some(Self::DatabaseUnreadable),
            4 => Some(Self::SourceIdentityUnavailable),
            _ => None,
        }
    }
}
