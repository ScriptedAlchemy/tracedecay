use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ManifestDigest, ProjectId, VectorGenerationIdV1, canonical_sha256};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::{
    ProductionSemanticRetrievalConfigurationStoreV1, SemanticActivationReceiptV1,
    SemanticConfigurationBackendErrorV1, SemanticCurrentLinkedActivationV1,
};

const INVENTORY_DIGEST_DOMAIN: &str = "tracedecay.semantic-retrieval.configuration-inventory.v1";
pub const MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE: u16 = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticConfigurationInventoryCursorV1 {
    project_id: ProjectId,
    store_binding_digest: ManifestDigest,
    revision: u64,
    after_scope_digest: ManifestDigest,
    cumulative_scope_count: u64,
    cumulative_root_binding_count: u64,
    cumulative_digest: ManifestDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticConfigurationInventoryReceiptV1 {
    project_id: ProjectId,
    store_binding_digest: ManifestDigest,
    revision: u64,
    scope_count: u64,
    root_binding_count: u64,
    inventory_digest: ManifestDigest,
}

impl SemanticConfigurationInventoryReceiptV1 {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn scope_count(&self) -> u64 {
        self.scope_count
    }

    pub fn root_binding_count(&self) -> u64 {
        self.root_binding_count
    }

    pub fn inventory_digest(&self) -> &ManifestDigest {
        &self.inventory_digest
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticConfigurationInventoryPageRequestV1 {
    after: Option<SemanticConfigurationInventoryCursorV1>,
    max_scopes: u16,
}

impl SemanticConfigurationInventoryPageRequestV1 {
    pub fn first(max_scopes: u16) -> Result<Self, SemanticConfigurationBackendErrorV1> {
        validate_page_size(max_scopes)?;
        Ok(Self {
            after: None,
            max_scopes,
        })
    }

    pub fn after(
        cursor: SemanticConfigurationInventoryCursorV1,
        max_scopes: u16,
    ) -> Result<Self, SemanticConfigurationBackendErrorV1> {
        validate_page_size(max_scopes)?;
        Ok(Self {
            after: Some(cursor),
            max_scopes,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticConfigurationInventoryPageV1 {
    pub scanned_scopes: u16,
    pub scanned_root_bindings: u16,
    pub continuation: Option<SemanticConfigurationInventoryCursorV1>,
    pub complete_receipt: Option<SemanticConfigurationInventoryReceiptV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticConfiguredVectorRootCursorV1 {
    project_id: ProjectId,
    store_binding_digest: ManifestDigest,
    revision: u64,
    configuration_inventory_digest: ManifestDigest,
    after_generation: VectorGenerationIdV1,
    cumulative_root_count: u64,
    cumulative_digest: ManifestDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticConfiguredVectorRootReceiptV1 {
    project_id: ProjectId,
    store_binding_digest: ManifestDigest,
    revision: u64,
    configuration_inventory_digest: ManifestDigest,
    root_count: u64,
    root_digest: ManifestDigest,
}

impl SemanticConfiguredVectorRootReceiptV1 {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn root_count(&self) -> u64 {
        self.root_count
    }

    pub fn root_digest(&self) -> &ManifestDigest {
        &self.root_digest
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticConfiguredVectorRootPageRequestV1 {
    inventory: Option<SemanticConfigurationInventoryReceiptV1>,
    after: Option<SemanticConfiguredVectorRootCursorV1>,
    max_roots: u16,
}

impl SemanticConfiguredVectorRootPageRequestV1 {
    pub fn first(
        inventory: SemanticConfigurationInventoryReceiptV1,
        max_roots: u16,
    ) -> Result<Self, SemanticConfigurationBackendErrorV1> {
        validate_page_size(max_roots)?;
        Ok(Self {
            inventory: Some(inventory),
            after: None,
            max_roots,
        })
    }

    pub fn after(
        cursor: SemanticConfiguredVectorRootCursorV1,
        max_roots: u16,
    ) -> Result<Self, SemanticConfigurationBackendErrorV1> {
        validate_page_size(max_roots)?;
        Ok(Self {
            inventory: None,
            after: Some(cursor),
            max_roots,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticConfiguredVectorRootPageV1 {
    pub roots: Vec<VectorGenerationIdV1>,
    pub continuation: Option<SemanticConfiguredVectorRootCursorV1>,
    pub complete_receipt: Option<SemanticConfiguredVectorRootReceiptV1>,
}

impl ProductionSemanticRetrievalConfigurationStoreV1 {
    pub async fn configuration_inventory_page(
        &self,
        request: &SemanticConfigurationInventoryPageRequestV1,
    ) -> Result<SemanticConfigurationInventoryPageV1, SemanticConfigurationBackendErrorV1> {
        validate_page_size(request.max_scopes)?;
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let project_id = self.scope().project_id.clone();
        let revision = inventory_revision(&snapshot, &project_id).await?;
        require_no_uncommitted_transition(&snapshot, &project_id).await?;
        let store_binding_digest = durable_store_binding_digest(self.database())?;
        let (after_scope, mut scope_count, mut root_binding_count, mut digest) =
            match request.after.as_ref() {
                Some(cursor)
                    if cursor.project_id == project_id
                        && cursor.store_binding_digest == store_binding_digest
                        && cursor.revision == revision =>
                {
                    (
                        cursor.after_scope_digest.as_str(),
                        cursor.cumulative_scope_count,
                        cursor.cumulative_root_binding_count,
                        cursor.cumulative_digest.clone(),
                    )
                }
                Some(_) => return Err(SemanticConfigurationBackendErrorV1::Conflict),
                None => (
                    "",
                    0,
                    0,
                    canonical_sha256(&(
                        INVENTORY_DIGEST_DOMAIN,
                        &project_id,
                        &store_binding_digest,
                        revision,
                    ))
                    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
                ),
            };
        let limit = u64::from(request.max_scopes)
            .checked_add(1)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
        let mut rows = snapshot
            .query(
                "SELECT state.scope_digest, state.scope_json, state.epoch,
                        state.configuration_revision, state.transition_digest,
                        state.activation_receipt_digest, state.active_vector_generation,
                        state.rollback_vector_generation, state.state_json,
                        state.activation_receipt_json
                   FROM configuration_semantic_retrieval_state_v1 AS state
                  WHERE state.scope_digest > ?1
                    AND state.project_id = ?2
                    AND NOT EXISTS (
                        SELECT 1
                          FROM configuration_semantic_retrieval_state_v1 AS newer
                         WHERE newer.project_id = state.project_id
                           AND newer.scope_digest = state.scope_digest
                           AND newer.epoch > state.epoch
                    )
                  ORDER BY state.scope_digest ASC
                  LIMIT ?3",
                params![after_scope, project_id.as_str(), limit],
            )
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let mut records = Vec::with_capacity(usize::from(request.max_scopes) + 1);
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
        {
            records.push(decode_inventory_record(&row, &project_id)?);
        }
        drop(rows);
        if records.is_empty() {
            return Err(SemanticConfigurationBackendErrorV1::Unavailable);
        }
        let has_more = records.len() > usize::from(request.max_scopes);
        records.truncate(usize::from(request.max_scopes));
        let mut page_root_bindings = 0_u16;
        for record in &records {
            scope_count = scope_count
                .checked_add(1)
                .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
            let roots = u64::from(record.active_vector_generation.is_some() as u8)
                + u64::from(record.rollback_vector_generation.is_some() as u8);
            root_binding_count = root_binding_count
                .checked_add(roots)
                .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
            page_root_bindings = page_root_bindings
                .checked_add(
                    u16::try_from(roots)
                        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
                )
                .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
            digest = canonical_sha256(&(
                INVENTORY_DIGEST_DOMAIN,
                &digest,
                &record.scope,
                record.epoch,
                &record.state_json,
                &record.transition_digest,
                &record.activation_receipt_digest,
                &record.active_vector_generation,
                &record.rollback_vector_generation,
            ))
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        }
        if inventory_revision(&snapshot, &project_id).await? != revision {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        let last_scope = records
            .last()
            .ok_or(SemanticConfigurationBackendErrorV1::Unavailable)?
            .scope
            .scope_digest
            .clone();
        let (continuation, complete_receipt) = if has_more {
            (
                Some(SemanticConfigurationInventoryCursorV1 {
                    project_id,
                    store_binding_digest,
                    revision,
                    after_scope_digest: last_scope,
                    cumulative_scope_count: scope_count,
                    cumulative_root_binding_count: root_binding_count,
                    cumulative_digest: digest,
                }),
                None,
            )
        } else {
            (
                None,
                Some(SemanticConfigurationInventoryReceiptV1 {
                    project_id,
                    store_binding_digest,
                    revision,
                    scope_count,
                    root_binding_count,
                    inventory_digest: digest,
                }),
            )
        };
        Ok(SemanticConfigurationInventoryPageV1 {
            scanned_scopes: u16::try_from(records.len())
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
            scanned_root_bindings: page_root_bindings,
            continuation,
            complete_receipt,
        })
    }

    pub async fn is_vector_generation_configured(
        &self,
        receipt: &SemanticConfiguredVectorRootReceiptV1,
        generation: &VectorGenerationIdV1,
    ) -> Result<bool, SemanticConfigurationBackendErrorV1> {
        generation
            .validate()
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        let expected_store = durable_store_binding_digest(self.database())?;
        if receipt.project_id != self.scope().project_id
            || receipt.store_binding_digest != expected_store
        {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        if inventory_revision(&snapshot, &receipt.project_id).await? != receipt.revision {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        require_no_uncommitted_transition(&snapshot, &receipt.project_id).await?;
        let mut rows = snapshot
            .query(
                "SELECT 1
                  FROM configuration_semantic_retrieval_state_v1 AS state
                  WHERE state.project_id = ?1
                    AND (state.active_vector_generation = ?2
                      OR state.rollback_vector_generation = ?2)
                    AND NOT EXISTS (
                        SELECT 1
                          FROM configuration_semantic_retrieval_state_v1 AS newer
                         WHERE newer.project_id = state.project_id
                           AND newer.scope_digest = state.scope_digest
                           AND newer.epoch > state.epoch
                    )
                  LIMIT 1",
                params![receipt.project_id.as_str(), generation.as_digest().as_str()],
            )
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let configured = rows
            .next()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
            .is_some();
        if inventory_revision(&snapshot, &receipt.project_id).await? != receipt.revision {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        Ok(configured)
    }

    pub async fn configured_vector_roots_page(
        &self,
        request: &SemanticConfiguredVectorRootPageRequestV1,
    ) -> Result<SemanticConfiguredVectorRootPageV1, SemanticConfigurationBackendErrorV1> {
        validate_page_size(request.max_roots)?;
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let project_id = self.scope().project_id.clone();
        let revision = inventory_revision(&snapshot, &project_id).await?;
        require_no_uncommitted_transition(&snapshot, &project_id).await?;
        let store_binding_digest = durable_store_binding_digest(self.database())?;
        let (after_generation, configuration_inventory_digest, mut root_count, mut digest) =
            match (&request.inventory, &request.after) {
                (Some(inventory), None)
                    if inventory.project_id == project_id
                        && inventory.store_binding_digest == store_binding_digest
                        && inventory.revision == revision =>
                {
                    (
                        "",
                        inventory.inventory_digest.clone(),
                        0,
                        canonical_sha256(&(
                            INVENTORY_DIGEST_DOMAIN,
                            "configured-vector-roots",
                            &project_id,
                            &store_binding_digest,
                            revision,
                            &inventory.inventory_digest,
                        ))
                        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
                    )
                }
                (None, Some(cursor))
                    if cursor.project_id == project_id
                        && cursor.store_binding_digest == store_binding_digest
                        && cursor.revision == revision =>
                {
                    (
                        cursor.after_generation.as_digest().as_str(),
                        cursor.configuration_inventory_digest.clone(),
                        cursor.cumulative_root_count,
                        cursor.cumulative_digest.clone(),
                    )
                }
                _ => return Err(SemanticConfigurationBackendErrorV1::Conflict),
            };
        let limit = u64::from(request.max_roots)
            .checked_add(1)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
        let mut rows = snapshot
            .query(
                "WITH latest AS (
                     SELECT state.active_vector_generation,
                            state.rollback_vector_generation
                       FROM configuration_semantic_retrieval_state_v1 AS state
                      WHERE state.project_id = ?1
                        AND NOT EXISTS (
                            SELECT 1
                              FROM configuration_semantic_retrieval_state_v1 AS newer
                             WHERE newer.project_id = state.project_id
                               AND newer.scope_digest = state.scope_digest
                               AND newer.epoch > state.epoch
                        )
                 ), configured(generation) AS (
                     SELECT active_vector_generation FROM latest
                      WHERE active_vector_generation IS NOT NULL
                     UNION
                     SELECT rollback_vector_generation FROM latest
                      WHERE rollback_vector_generation IS NOT NULL
                 )
                 SELECT generation
                   FROM configured
                  WHERE generation > ?2
                  ORDER BY generation ASC
                  LIMIT ?3",
                params![project_id.as_str(), after_generation, limit],
            )
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let mut roots = Vec::with_capacity(usize::from(request.max_roots) + 1);
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
        {
            let generation = VectorGenerationIdV1::new(
                ManifestDigest::new(
                    row.get::<String>(0)
                        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
                )
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
            );
            generation
                .validate()
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            roots.push(generation);
        }
        drop(rows);
        let has_more = roots.len() > usize::from(request.max_roots);
        roots.truncate(usize::from(request.max_roots));
        for root in &roots {
            root_count = root_count
                .checked_add(1)
                .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
            digest = canonical_sha256(&(
                INVENTORY_DIGEST_DOMAIN,
                "configured-vector-root",
                &digest,
                root,
            ))
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        }
        if inventory_revision(&snapshot, &project_id).await? != revision {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        if has_more {
            let after_generation = roots
                .last()
                .cloned()
                .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
            return Ok(SemanticConfiguredVectorRootPageV1 {
                roots,
                continuation: Some(SemanticConfiguredVectorRootCursorV1 {
                    project_id,
                    store_binding_digest,
                    revision,
                    configuration_inventory_digest,
                    after_generation,
                    cumulative_root_count: root_count,
                    cumulative_digest: digest,
                }),
                complete_receipt: None,
            });
        }
        Ok(SemanticConfiguredVectorRootPageV1 {
            roots,
            continuation: None,
            complete_receipt: Some(SemanticConfiguredVectorRootReceiptV1 {
                project_id,
                store_binding_digest,
                revision,
                configuration_inventory_digest,
                root_count,
                root_digest: digest,
            }),
        })
    }
}

#[derive(Debug)]
struct InventoryRecord {
    scope: ResolvedScope,
    epoch: i64,
    state_json: String,
    transition_digest: Option<ManifestDigest>,
    activation_receipt_digest: Option<ManifestDigest>,
    active_vector_generation: Option<String>,
    rollback_vector_generation: Option<String>,
}

fn decode_inventory_record(
    row: &tracedecay_runtime_core::db::engine::Row,
    project_id: &ProjectId,
) -> Result<InventoryRecord, SemanticConfigurationBackendErrorV1> {
    let scope_digest = ManifestDigest::new(
        row.get::<String>(0)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )
    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let scope = super::config_store::decode_scope(
        &row.get::<String>(1)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )?;
    if scope.scope_digest != scope_digest || &scope.project_id != project_id {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    let epoch = row
        .get::<i64>(2)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    if epoch < 0 {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    let configuration_revision = row
        .get::<String>(3)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let transition_digest = super::config_store::decode_optional_digest(
        row.get::<Option<String>>(4)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )?;
    let activation_receipt_digest = super::config_store::decode_optional_digest(
        row.get::<Option<String>>(5)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )?;
    let active_vector_generation = row
        .get::<Option<String>>(6)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let rollback_vector_generation = row
        .get::<Option<String>>(7)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let state_json = row
        .get::<String>(8)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let state = super::config_store::decode_state(&state_json)?;
    if state.configuration_revision().as_str() != configuration_revision {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    super::config_store::validate_normalized_semantic_vector_roots(
        &state,
        active_vector_generation.as_deref(),
        rollback_vector_generation.as_deref(),
    )?;
    let receipt = row
        .get::<Option<String>>(9)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?
        .map(|json| {
            let receipt: SemanticActivationReceiptV1 = serde_json::from_str(&json)
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            receipt
                .validate()
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            Ok(receipt)
        })
        .transpose()?;
    if receipt.as_ref().map(|value| &value.receipt_digest) != activation_receipt_digest.as_ref() {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    match (
        state.active().compatibility().semantic.as_ref(),
        receipt.as_ref(),
    ) {
        (Some(compatibility), Some(receipt)) => {
            if receipt.configuration.revision_id != *state.configuration_revision() {
                return Err(SemanticConfigurationBackendErrorV1::Rejected);
            }
            SemanticCurrentLinkedActivationV1::new(receipt.clone(), compatibility.clone())
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        }
        (None, None) => {}
        _ => return Err(SemanticConfigurationBackendErrorV1::Rejected),
    }
    match transition_digest.as_ref() {
        None if epoch == 0 && state.audit().is_empty() => {}
        Some(_) if epoch > 0 && !state.audit().is_empty() => {}
        _ => return Err(SemanticConfigurationBackendErrorV1::Rejected),
    }
    Ok(InventoryRecord {
        scope,
        epoch,
        state_json,
        transition_digest,
        activation_receipt_digest,
        active_vector_generation,
        rollback_vector_generation,
    })
}

async fn inventory_revision(
    executor: &impl QueryExecutor,
    project_id: &ProjectId,
) -> Result<u64, SemanticConfigurationBackendErrorV1> {
    let mut rows = executor
        .query(
            "SELECT revision
              FROM configuration_semantic_retrieval_inventory_v1
              WHERE project_id = ?1",
            params![project_id.as_str()],
        )
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
        .ok_or(SemanticConfigurationBackendErrorV1::Unavailable)?;
    let revision = row
        .get::<i64>(0)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    if rows
        .next()
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
        .is_some()
    {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    u64::try_from(revision).map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)
}

async fn require_no_uncommitted_transition(
    executor: &impl QueryExecutor,
    project_id: &ProjectId,
) -> Result<(), SemanticConfigurationBackendErrorV1> {
    let mut rows = executor
        .query(
            "SELECT 1
              FROM configuration_semantic_retrieval_pending_v1 AS pending
              WHERE pending.project_id = ?1
                AND NOT EXISTS (
                    SELECT 1
                      FROM configuration_semantic_retrieval_state_v1 AS state
                     WHERE state.project_id = pending.project_id
                       AND state.scope_digest = pending.scope_digest
                       AND state.transition_digest = pending.transition_digest
              )
              LIMIT 1",
            params![project_id.as_str()],
        )
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
    if rows
        .next()
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
        .is_some()
    {
        return Err(SemanticConfigurationBackendErrorV1::Unavailable);
    }
    Ok(())
}

fn validate_page_size(max_scopes: u16) -> Result<(), SemanticConfigurationBackendErrorV1> {
    if max_scopes == 0 || max_scopes > MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    Ok(())
}

fn durable_store_binding_digest(
    database: &tracedecay_global_db::RegisteredGlobalDb,
) -> Result<ManifestDigest, SemanticConfigurationBackendErrorV1> {
    canonical_sha256(&(
        INVENTORY_DIGEST_DOMAIN,
        "durable-store",
        &database.binding().shard_id,
        &database.runtime().locator().verified().locator_digest,
    ))
    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)
}

#[cfg(test)]
#[path = "config_inventory_tests.rs"]
mod tests;
