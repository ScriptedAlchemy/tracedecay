use std::collections::BTreeSet;

use tracedecay_domain::{CodeGenerationId, ManifestDigest, canonical_sha256};
use tracedecay_runtime_core::db::DatabaseWriteTransaction;
use tracedecay_semantic::legacy_migration::{
    LegacyVectorInventoryPortV1, LegacyVectorInventoryV1, LegacyVectorMigrationErrorV1,
};

use super::vector_generations::VectorGenerationStoreErrorV1;

const RETENTION_SOURCE_DIGEST_DOMAIN_V1: &str =
    "tracedecay.vector-generation-retention-source-witness.v1";

/// Identity-only snapshot of the legacy state. The SQL adapter never returns
/// legacy vector payloads to Rust.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseLegacyVectorInventoryV1 {
    revision: i64,
    inventory: LegacyVectorInventoryV1,
}

impl DatabaseLegacyVectorInventoryV1 {
    pub(super) const fn new(revision: i64, inventory: LegacyVectorInventoryV1) -> Self {
        Self {
            revision,
            inventory,
        }
    }

    pub(super) const fn revision(&self) -> i64 {
        self.revision
    }

    pub(super) const fn inventory(&self) -> &LegacyVectorInventoryV1 {
        &self.inventory
    }

    pub fn retention_witness(
        &self,
    ) -> Result<VectorGenerationRetentionWitnessV1, VectorGenerationStoreErrorV1> {
        let readable_sources = self.inventory.retained_readable_sources();
        Ok(VectorGenerationRetentionWitnessV1 {
            revision: self.revision,
            source_digest: source_digest(&readable_sources)?,
            readable_sources,
        })
    }
}

impl LegacyVectorInventoryPortV1 for DatabaseLegacyVectorInventoryV1 {
    fn read_only_inventory(&self) -> Result<LegacyVectorInventoryV1, LegacyVectorMigrationErrorV1> {
        Ok(self.inventory.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VectorGenerationRetentionWitnessV1 {
    revision: i64,
    source_digest: ManifestDigest,
    readable_sources: BTreeSet<CodeGenerationId>,
}

impl VectorGenerationRetentionWitnessV1 {
    pub fn readable_sources(&self) -> &BTreeSet<CodeGenerationId> {
        &self.readable_sources
    }

    pub async fn validate(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        let (revision, readable_sources) = read_retention_identity(transaction).await?;
        if revision != self.revision || source_digest(&readable_sources)? != self.source_digest {
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        Ok(())
    }
}

async fn read_retention_identity(
    transaction: &DatabaseWriteTransaction<'_>,
) -> Result<(i64, BTreeSet<CodeGenerationId>), VectorGenerationStoreErrorV1> {
    let mut rows = transaction
        .query_engine(
            "SELECT state.revision,
                    json_type(state.state_json, '$.published.generations'),
                    entry.key,
                    entry.type,
                    CASE WHEN entry.type = 'object'
                         THEN CAST(json_extract(entry.value, '$.generation_id') AS TEXT)
                    END,
                    CASE WHEN entry.type = 'object'
                         THEN CAST(json_extract(entry.value, '$.source_generation') AS TEXT)
                    END
             FROM semantic_vector_generation_state_v1 AS state
             LEFT JOIN json_each(
                 state.state_json,
                 '$.published.generations'
             ) AS entry
             WHERE state.singleton = 1
             ORDER BY entry.key",
            (),
        )
        .await
        .map_err(store_error)?;
    let mut revision = None;
    let mut readable_sources = BTreeSet::new();
    while let Some(row) = rows.next().await.map_err(store_error)? {
        let row_revision = row.get::<i64>(0).map_err(store_error)?;
        if revision
            .replace(row_revision)
            .is_some_and(|prior| prior != row_revision)
        {
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        if row
            .get::<Option<String>>(1)
            .map_err(store_error)?
            .as_deref()
            != Some("object")
        {
            return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                "legacy generation inventory is not a JSON object".to_owned(),
            ));
        }
        let Some(map_key) = row.get::<Option<String>>(2).map_err(store_error)? else {
            continue;
        };
        let value_type = row.get::<Option<String>>(3).map_err(store_error)?;
        let embedded_generation = row.get::<Option<String>>(4).map_err(store_error)?;
        let source_generation = row.get::<Option<String>>(5).map_err(store_error)?;
        if value_type.as_deref() == Some("object")
            && embedded_generation.as_deref() == Some(map_key.as_str())
            && let Some(source_generation) =
                source_generation.and_then(|raw| CodeGenerationId::try_from(raw).ok())
        {
            readable_sources.insert(source_generation);
        }
    }
    drop(rows);
    let revision = revision.ok_or_else(|| {
        VectorGenerationStoreErrorV1::Storage("vector generation state row is missing".to_owned())
    })?;
    Ok((revision, readable_sources))
}

fn source_digest(
    readable_sources: &BTreeSet<CodeGenerationId>,
) -> Result<ManifestDigest, VectorGenerationStoreErrorV1> {
    canonical_sha256(&(RETENTION_SOURCE_DIGEST_DOMAIN_V1, readable_sources))
        .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))
}

fn store_error(error: impl std::fmt::Display) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Storage(error.to_string())
}
