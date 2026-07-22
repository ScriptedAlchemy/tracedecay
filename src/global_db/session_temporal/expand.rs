//! Generation-bound derived evidence member expansion.

use libsql::params;
use tracedecay_domain::{
    DerivedEvidenceIdV1, DerivedEvidenceKindV1, DerivedEvidenceMemberRoleV1, MessageOccurrenceIdV1,
    SessionCursorKeyIdV1, SessionCursorVersionV1, SessionProjectionGenerationV1,
    SignedCursorKeyRefV1, TemporalCoverageCountsV1, UtcMicros,
};
use tracedecay_store::{
    DerivedEvidenceMemberPageItemV1, DerivedEvidenceMemberPageV1,
    MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE, SessionFrozenWatermarksV1, SessionRetrievalPageV1,
    SessionStoreError, SessionStoreResult, SessionTemporalCapabilitiesV1,
    SessionTemporalCapabilityV1, SessionTemporalRetrievalRequestV1, SessionTemporalSnapshotRequestV1,
    SessionTemporalSnapshotV1,
};

use super::query::{storage, storage_message};
use crate::global_db::GlobalDb;

const EXPAND_OPERATION: &str = "expand session derived evidence members";
const FREEZE_OPERATION: &str = "freeze session temporal snapshot";

impl GlobalDb {
    pub(crate) async fn freeze_session_temporal_snapshot_result(
        &self,
        request: SessionTemporalSnapshotRequestV1,
    ) -> SessionStoreResult<SessionTemporalSnapshotV1> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(FREEZE_OPERATION, error))?;
        let mut rows = snapshot
            .query(
                "SELECT generation, frozen_watermarks_json
                 FROM session_temporal_generations
                 WHERE session_id = ?1 AND state = 'active'
                 LIMIT 2",
                params![request.session_id().as_str()],
            )
            .await
            .map_err(|error| storage(FREEZE_OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| storage(FREEZE_OPERATION, error))?
            .ok_or_else(|| {
                storage_message(
                    FREEZE_OPERATION,
                    "active session temporal generation is missing",
                )
            })?;
        if rows
            .next()
            .await
            .map_err(|error| storage(FREEZE_OPERATION, error))?
            .is_some()
        {
            return Err(storage_message(
                FREEZE_OPERATION,
                "active session temporal generation is not unique",
            ));
        }
        let generation: i64 = row
            .get(0)
            .map_err(|error| storage(FREEZE_OPERATION, error))?;
        let encoded: String = row
            .get(1)
            .map_err(|error| storage(FREEZE_OPERATION, error))?;
        let watermarks = decode_frozen_watermarks(&encoded)?;
        if i64::try_from(watermarks.active_generation().value())
            .map_err(|error| storage(FREEZE_OPERATION, error))?
            != generation
        {
            return Err(storage_message(
                FREEZE_OPERATION,
                "active generation disagrees with frozen watermarks",
            ));
        }
        Ok(SessionTemporalSnapshotV1::new(
            request.session_id().clone(),
            UtcMicros(0),
            watermarks,
            SessionTemporalCapabilitiesV1::new([
                SessionTemporalCapabilityV1::FrozenWatermarks,
                SessionTemporalCapabilityV1::GenerationRebuild,
            ]),
        ))
    }

    pub(crate) async fn expand_derived_members_result(
        &self,
        snapshot: SessionTemporalSnapshotV1,
        evidence_kind: DerivedEvidenceKindV1,
        evidence_id: DerivedEvidenceIdV1,
        after_ordinal: Option<u32>,
        limit: usize,
    ) -> SessionStoreResult<DerivedEvidenceMemberPageV1> {
        if !(1..=MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE).contains(&limit) {
            return Err(SessionStoreError::InvalidPageLimit {
                limit,
                max: MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE,
            });
        }
        let generation = i64::try_from(snapshot.watermarks().active_generation().value())
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
        let read = self
            .read_snapshot()
            .await
            .map_err(|error| storage(EXPAND_OPERATION, error))?;

        let mut evidence_rows = read
            .query(
                "SELECT 1
                 FROM session_derived_evidence
                 WHERE session_id = ?1
                   AND generation = ?2
                   AND evidence_kind = ?3
                   AND evidence_id = ?4
                 LIMIT 1",
                params![
                    snapshot.session_id().as_str(),
                    generation,
                    evidence_kind.as_str(),
                    evidence_id.as_str(),
                ],
            )
            .await
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
        if evidence_rows
            .next()
            .await
            .map_err(|error| storage(EXPAND_OPERATION, error))?
            .is_none()
        {
            return Err(storage_message(
                EXPAND_OPERATION,
                "derived evidence record is missing from the frozen generation",
            ));
        }

        let after = after_ordinal.map_or(-1_i64, i64::from);
        let fetch_limit = i64::try_from(limit.saturating_add(1))
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
        let mut rows = read
            .query(
                "SELECT member.ordinal, member.occurrence_id, member.member_role,
                        occurrence.occurrence_id IS NOT NULL
                 FROM session_derived_evidence_members AS member
                 LEFT JOIN session_occurrences AS occurrence
                   ON occurrence.session_id = member.session_id
                  AND occurrence.generation = member.generation
                  AND occurrence.occurrence_id = member.occurrence_id
                 WHERE member.session_id = ?1
                   AND member.generation = ?2
                   AND member.evidence_kind = ?3
                   AND member.evidence_id = ?4
                   AND member.ordinal > ?5
                 ORDER BY member.ordinal ASC
                 LIMIT ?6",
                params![
                    snapshot.session_id().as_str(),
                    generation,
                    evidence_kind.as_str(),
                    evidence_id.as_str(),
                    after,
                    fetch_limit,
                ],
            )
            .await
            .map_err(|error| storage(EXPAND_OPERATION, error))?;

        let mut members = Vec::new();
        let mut next_after_ordinal = None;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(EXPAND_OPERATION, error))?
        {
            if members.len() == limit {
                next_after_ordinal = members
                    .last()
                    .map(|item: &DerivedEvidenceMemberPageItemV1| item.ordinal);
                break;
            }
            let ordinal = u32::try_from(
                row.get::<i64>(0)
                    .map_err(|error| storage(EXPAND_OPERATION, error))?,
            )
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
            let occurrence_id = MessageOccurrenceIdV1::new(
                row.get::<String>(1)
                    .map_err(|error| storage(EXPAND_OPERATION, error))?,
            )?;
            let role_raw: String = row
                .get(2)
                .map_err(|error| storage(EXPAND_OPERATION, error))?;
            let member_role = match role_raw.as_str() {
                "member" => DerivedEvidenceMemberRoleV1::Member,
                "first" => DerivedEvidenceMemberRoleV1::First,
                "last" => DerivedEvidenceMemberRoleV1::Last,
                _ => {
                    return Err(storage_message(
                        EXPAND_OPERATION,
                        format!("unknown derived member role: {role_raw}"),
                    ));
                }
            };
            let available = row
                .get::<i64>(3)
                .map_err(|error| storage(EXPAND_OPERATION, error))?
                != 0;
            members.push(DerivedEvidenceMemberPageItemV1 {
                ordinal,
                occurrence_id,
                member_role,
                available,
            });
        }

        DerivedEvidenceMemberPageV1::new(evidence_id, evidence_kind, members, next_after_ordinal)
    }

    pub(crate) async fn retrieve_session_temporal_page_result(
        &self,
        request: SessionTemporalRetrievalRequestV1,
    ) -> SessionStoreResult<SessionRetrievalPageV1> {
        // Compact occurrence/summary paging stays on TemporalReadPort. This
        // store port remains capability-gated and returns an empty typed page.
        let _ = self;
        SessionRetrievalPageV1::new(
            request.snapshot().clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            TemporalCoverageCountsV1 {
                visible: 0,
                hidden: 0,
                unknown: 0,
                redacted: 0,
            },
            None,
        )
    }
}

fn decode_frozen_watermarks(encoded: &str) -> SessionStoreResult<SessionFrozenWatermarksV1> {
    let value: serde_json::Value =
        serde_json::from_str(encoded).map_err(|error| storage(FREEZE_OPERATION, error))?;
    let generation = value["active_generation"]
        .as_u64()
        .ok_or_else(|| storage_message(FREEZE_OPERATION, "active_generation is invalid"))?;
    let source = value["source_frontier"]
        .as_u64()
        .ok_or_else(|| storage_message(FREEZE_OPERATION, "source_frontier is invalid"))?;
    let projection = value["projection_frontier"]
        .as_u64()
        .ok_or_else(|| storage_message(FREEZE_OPERATION, "projection_frontier is invalid"))?;
    let summary = value["summary_frontier"]
        .as_u64()
        .ok_or_else(|| storage_message(FREEZE_OPERATION, "summary_frontier is invalid"))?;
    let mut watermarks = SessionFrozenWatermarksV1::new(
        SessionProjectionGenerationV1::new(generation)?,
        source,
        projection,
        summary,
    );
    if let Some(cursor) = value.get("cursor_key").filter(|value| !value.is_null()) {
        let key_id = cursor["key_id"]
            .as_str()
            .ok_or_else(|| storage_message(FREEZE_OPERATION, "cursor key id is invalid"))?;
        let version = cursor["version"]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| storage_message(FREEZE_OPERATION, "cursor key version is invalid"))?;
        watermarks = watermarks.with_cursor_key(SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new(key_id)?,
            version: SessionCursorVersionV1::new(version)?,
        });
    }
    Ok(watermarks)
}
