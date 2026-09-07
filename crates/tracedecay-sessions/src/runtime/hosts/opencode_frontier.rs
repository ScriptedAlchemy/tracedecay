use tracedecay_domain::ObservationScopeV1;
use tracedecay_store::ParseOffset;

use crate::admission::HostAdmission;
use crate::runtime::source::TranscriptIngestResult;

const PROVIDER: &str = "opencode";
pub(super) const GENERATION_KEY: &str = "host-frontier://opencode/content-generation/v1";
pub(super) const REWRITE_KEY: &str = "host-frontier://opencode/rewrite-rowid/v1";

pub(super) async fn prepare_generation_rewrite(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    current_generation: u64,
    source_file_identity: u64,
) -> TranscriptIngestResult<(ParseOffset, ParseOffset)> {
    let mut generation = read(admission, scope, GENERATION_KEY).await?;
    let mut rewrite = read(admission, scope, REWRITE_KEY).await?;
    if generation.file_id != source_file_identity {
        let revision = generation.mtime.max(rewrite.mtime).saturating_add(1).max(1);
        generation = ParseOffset {
            byte_offset: current_generation,
            mtime: revision,
            file_id: source_file_identity,
        };
        rewrite = ParseOffset {
            byte_offset: u64::MAX,
            mtime: revision,
            file_id: source_file_identity,
        };
        write(admission, scope, GENERATION_KEY, generation).await?;
        write(admission, scope, REWRITE_KEY, rewrite).await?;
    } else if generation.byte_offset != current_generation && rewrite.byte_offset == u64::MAX {
        let revision = generation.mtime.max(rewrite.mtime).saturating_add(1).max(1);
        generation = ParseOffset {
            byte_offset: current_generation,
            mtime: revision,
            file_id: source_file_identity,
        };
        rewrite = ParseOffset {
            byte_offset: 0,
            mtime: revision,
            file_id: source_file_identity,
        };
        write(admission, scope, GENERATION_KEY, generation).await?;
        write(admission, scope, REWRITE_KEY, rewrite).await?;
    }
    Ok((generation, rewrite))
}

pub(super) async fn read(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    key: &str,
) -> TranscriptIngestResult<ParseOffset> {
    admission
        .get_parse_offset(scope, key)
        .await
        .map(|frontier| frontier.unwrap_or_default())
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
        })
}

pub(super) async fn write(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    key: &str,
    frontier: ParseOffset,
) -> TranscriptIngestResult<()> {
    admission
        .advance_parse_offset(scope, key, frontier)
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
        })
}
