use tracedecay_runtime_core::db::engine::Executor;
use tracedecay_runtime_core::db::{
    MemoryV2ArchiveDatabase, export_memory_v2_owner_archive, import_memory_v2_owner_archive,
    list_memory_v2_archive_owners, plan_memory_v2_owner_archive_import,
};

use serde::{Deserialize, Serialize};
use tracedecay_domain::FactOwnerV1;
use tracedecay_runtime_core::errors::Result;
use tracedecay_store::MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1;

use super::{db_error, db_message};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryV2ArchiveMergeProof {
    pub owner: FactOwnerV1,
    pub schema: String,
    pub source_digest: String,
    pub target_digest: String,
}

/// Transfers every owner-scoped Memory V2 closure through the typed archive
/// port. Planning observes the target before mutation, import preserves exact
/// stable identities, and readback verification occurs in the same target
/// transaction.
pub(super) async fn merge_memory_v2_owner_archives(
    conn: &(impl Executor + Sync),
) -> Result<Vec<MemoryV2ArchiveMergeProof>> {
    if !source_has_memory_v2(conn).await? {
        return Ok(Vec::new());
    }
    let owners = list_memory_v2_archive_owners(conn, MemoryV2ArchiveDatabase::Source).await?;
    let mut proofs = Vec::with_capacity(owners.len());
    for owner in owners {
        let archive =
            export_memory_v2_owner_archive(conn, MemoryV2ArchiveDatabase::Source, &owner).await?;
        let plan = plan_memory_v2_owner_archive_import(conn, &archive).await?;
        import_memory_v2_owner_archive(conn, &archive, &plan).await?;
        let target =
            export_memory_v2_owner_archive(conn, MemoryV2ArchiveDatabase::Main, &owner).await?;
        proofs.push(MemoryV2ArchiveMergeProof {
            owner,
            schema: MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1.to_owned(),
            source_digest: archive
                .digest()
                .map_err(|error| db_message("memory_v2_owner_archive", error.to_string()))?
                .as_str()
                .to_owned(),
            target_digest: target
                .digest()
                .map_err(|error| db_message("memory_v2_owner_archive", error.to_string()))?
                .as_str()
                .to_owned(),
        });
    }
    Ok(proofs)
}

async fn source_has_memory_v2(conn: &impl Executor) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM source.sqlite_master
             WHERE type = 'table' AND name = 'memory_v2_facts'",
            (),
        )
        .await
        .map_err(|error| db_error("merge_memory_v2_probe", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| db_error("merge_memory_v2_probe", error))?
        .is_some())
}
