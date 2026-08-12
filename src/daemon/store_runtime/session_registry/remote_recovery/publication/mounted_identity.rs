use std::path::Path;

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_store::{StoreIncarnationV1, StoreShardIdV1};

use super::super::{Result, session_registry_error};

pub(super) fn validate_existing_mounted_identity(
    database: &RegisteredGlobalDb,
    expected_shard: &StoreShardIdV1,
    expected_incarnation: StoreIncarnationV1,
    expected_opened_file_identity: u64,
    expected_destination: &Path,
) -> Result<()> {
    let observed = database.runtime().opened_file_identity();
    let current = tracedecay_runtime_core::db::sqlite_generation_identity(database.db_path()).ok();
    let graph_identity_matches =
        database
            .session_relation_graph_identity()
            .is_ok_and(|(binding, locator)| {
                binding == database.binding()
                    && locator.shard_id == binding.shard_id
                    && locator.incarnation == binding.incarnation
            });
    if database.binding().shard_id == *expected_shard
        && database.binding().incarnation == expected_incarnation
        && observed == Some(expected_opened_file_identity)
        && current == Some(expected_opened_file_identity)
        && database.db_path() == expected_destination
        && graph_identity_matches
    {
        return Ok(());
    }
    Err(session_registry_error(
        "reuse mounted remote restore target",
        format!(
            "mounted binding, graph, or identity is not the restored ProjectSessions authority: opened={observed:?}, current={current:?}, expected={expected_opened_file_identity}"
        ),
    ))
}
