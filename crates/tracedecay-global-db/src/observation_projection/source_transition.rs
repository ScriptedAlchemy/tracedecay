use std::collections::BTreeSet;

use tracedecay_domain::{
    CanonicalObservationIdV1, ClineTranscriptStream, DurableObservationV1, FactOwnerV1,
    ObservationIdentityMaterialV1, RetrievalAnchorRecordV2, RetrievalAnchorTargetV2,
    cline_task_native_observation_id, prove_cline_native_source_transition,
};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{
    AnchorDispositionReasonClassV1, AnchorDispositionStateV1, ProjectionSkipReason,
    ProjectionStoreError, ProjectionStoreResult, RetrievalAnchorDispositionRecordV1,
    SESSION_MESSAGE_PROJECTOR_VERSION,
};

use super::apply::{
    derive_projection_with_alias, verify_effect, verify_observation_provider_usage,
    verify_provenance, verify_workflow_effects,
};
use super::state::{
    read_observation, read_output_authorities, reaggregate_output_state_for_output,
    resolve_output_projection, storage, verify_projection_rows,
};

#[derive(Clone, Copy)]
pub(super) enum SourceTransitionTarget<'a> {
    Live,
    Staged(&'a str),
}

/// Resolve immutable successor evidence; a disposition label alone is never authority.
pub(crate) async fn verify_native_source_supersession(
    conn: &impl QueryExecutor,
    predecessor: &DurableObservationV1,
) -> ProjectionStoreResult<()> {
    let successor = read_native_source_successor(conn, predecessor)
        .await?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    let effect = Box::pin(derive_projection_with_alias(conn, &successor)).await?;
    if matches!(effect.skip_reason(), Some(reason) if reason != ProjectionSkipReason::NonConversationalRecord)
    {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    if effect.skip_reason().is_some() {
        Box::pin(verify_effect(conn, &successor, &effect)).await?;
    } else {
        verify_observation_provider_usage(conn, &successor).await?;
        // Reopen audits use read-only connections without the live writer's
        // temporary cache. Resolve retained ownership through the same bounded
        // authority used by the canonical projection audit.
        let outputs = effect
            .messages()
            .map(|projection| {
                (
                    projection.message().provider.clone(),
                    projection.message().message_id.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        let authorities = read_output_authorities(conn, &outputs).await?;
        for projection in effect.messages() {
            verify_provenance(conn, projection).await?;
            let message = projection.message();
            let authority = authorities
                .get(&(message.provider.clone(), message.message_id.clone()))
                .ok_or(ProjectionStoreError::ProvenanceCollision)?;
            let owner = resolve_output_projection(
                conn,
                authority,
                Some((successor.observation_id().as_str(), &effect)),
                projection,
            )
            .await?;
            verify_provenance(conn, &owner).await?;
            verify_projection_rows(conn, &owner).await?;
        }
        verify_workflow_effects(conn, effect.workflow_facts()).await?;
    }
    let (old_anchor, new_anchor) = read_transition_anchors(conn, predecessor, &successor).await?;
    let owner = serde_json::to_string(old_anchor.owner())
        .map_err(|error| storage("encode native source owner", error))?;
    let mut disposition = conn
        .query(
            "SELECT state, superseded_by, reason_class FROM retrieval_anchor_dispositions
         WHERE anchor_id = ?1 AND owner_json = ?2 ORDER BY sequence DESC LIMIT 1",
            params![old_anchor.anchor_id().as_str(), owner.as_str()],
        )
        .await
        .map_err(|error| storage("verify native source anchor supersession", error))?;
    let current = disposition
        .next()
        .await
        .map_err(|error| storage("verify native source anchor supersession", error))?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    if current
        .get::<String>(0)
        .map_err(|error| storage("verify native source anchor supersession", error))?
        != "superseded"
        || current
            .get::<Option<String>>(1)
            .map_err(|error| storage("verify native source anchor supersession", error))?
            .as_deref()
            != Some(new_anchor.anchor_id().as_str())
        || current
            .get::<String>(2)
            .map_err(|error| storage("verify native source anchor supersession", error))?
            != "correction"
    {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    drop(disposition);
    let mut aliases = conn.query(
        "SELECT COUNT(*) FROM retrieval_anchor_aliases WHERE owner_json = ?1 AND anchor_id = ?2",
        params![owner, new_anchor.anchor_id().as_str()],
    ).await.map_err(|error| storage("verify native source current alias", error))?;
    let count: i64 = aliases
        .next()
        .await
        .map_err(|error| storage("verify native source current alias", error))?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?
        .get(0)
        .map_err(|error| storage("verify native source current alias", error))?;
    if usize::try_from(count).ok() != Some(new_anchor.aliases().len()) {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    drop(aliases);
    let mut rows = conn.query(
            "SELECT 1 FROM observation_projection_provenance WHERE projector_version = ?1 AND observation_id = ?2
             UNION ALL SELECT 1 FROM observation_workflow_facts WHERE projector_version = ?1 AND observation_id = ?2
             UNION ALL SELECT 1 FROM observation_projection_aliases WHERE projector_version = ?1 AND observation_id = ?2
             LIMIT 1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, predecessor.observation_id().as_str()],
        ).await.map_err(|error| storage("verify superseded native source effects", error))?;
    if rows
        .next()
        .await
        .map_err(|error| storage("verify superseded native source effects", error))?
        .is_some()
    {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    drop(rows);

    Ok(())
}

async fn read_native_source_successor(
    conn: &impl QueryExecutor,
    predecessor: &DurableObservationV1,
) -> ProjectionStoreResult<Option<DurableObservationV1>> {
    if predecessor.source().explicit_source_key().is_some() {
        return Ok(None);
    }
    let identity = predecessor.identity();
    let native = identity
        .native_record_id()
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    for stream in [
        ClineTranscriptStream::ApiHistory,
        ClineTranscriptStream::UiMessages,
    ] {
        let source = stream
            .source_identity(
                predecessor.source().provider().clone(),
                predecessor.source().session_id().clone(),
            )
            .map_err(ProjectionStoreError::Contract)?;
        let material = ObservationIdentityMaterialV1::for_native_record(
            source,
            predecessor.scope().clone(),
            identity.generation(),
            identity.position(),
            identity.ordering_domain(),
            native.clone(),
        )
        .map_err(ProjectionStoreError::Contract)?;
        let id =
            CanonicalObservationIdV1::derive(&material).map_err(ProjectionStoreError::Contract)?;
        let Some((_, successor)) = read_observation(conn, &id).await? else {
            continue;
        };
        if prove_cline_native_source_transition(predecessor, &successor).is_none() {
            continue;
        }
        return Ok(Some(successor));
    }
    Ok(None)
}

pub(super) async fn read_native_source_predecessor(
    conn: &impl QueryExecutor,
    successor: &DurableObservationV1,
) -> ProjectionStoreResult<Option<DurableObservationV1>> {
    let Some(id) =
        cline_task_native_observation_id(successor).map_err(ProjectionStoreError::Contract)?
    else {
        return Ok(None);
    };
    let Some((_, predecessor)) = read_observation(conn, &id).await? else {
        return Ok(None);
    };
    if prove_cline_native_source_transition(&predecessor, successor).is_none() {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    Ok(Some(predecessor))
}

/// Runs after the successor effect succeeds, within its existing savepoint.
/// Staging removes only staged effects; live authority changes at activation.
pub(super) async fn settle_native_source_transition(
    conn: &impl Executor,
    successor: &DurableObservationV1,
    target: SourceTransitionTarget<'_>,
) -> ProjectionStoreResult<()> {
    let Some(predecessor) = read_native_source_predecessor(conn, successor).await? else {
        return Ok(());
    };
    let (provenance, workflow, dispositions, generation) = match target {
        SourceTransitionTarget::Live => (
            "observation_projection_provenance",
            "observation_workflow_facts",
            "observation_projection_dispositions",
            None,
        ),
        SourceTransitionTarget::Staged(generation) => (
            "observation_projection_rebuild_provenance",
            "observation_projection_rebuild_workflow_facts",
            "observation_projection_rebuild_dispositions",
            Some(generation),
        ),
    };
    let generation_filter = if generation.is_some() {
        " AND generation = ?4"
    } else {
        " AND ?4 IS NULL"
    };
    let old = predecessor.observation_id().as_str();
    let new = successor.observation_id().as_str();
    let mut affected = Vec::new();
    let mut rows = conn
        .query(
            &format!(
                "SELECT prior.output_provider, prior.output_message_id, next.observation_id
            FROM {provenance} AS prior
            LEFT JOIN {provenance} AS next ON next.projector_version = prior.projector_version
              AND next.observation_id = ?3 AND next.output_provider = prior.output_provider
              AND next.output_message_id = prior.output_message_id{}
            WHERE prior.projector_version = ?1 AND prior.observation_id = ?2{}",
                if generation.is_some() {
                    " AND next.generation = prior.generation"
                } else {
                    ""
                },
                if generation.is_some() {
                    " AND prior.generation = ?4"
                } else {
                    " AND ?4 IS NULL"
                }
            ),
            params![SESSION_MESSAGE_PROJECTOR_VERSION, old, new, generation],
        )
        .await
        .map_err(|error| storage("read native source predecessor outputs", error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read native source predecessor outputs", error))?
    {
        if row
            .get::<Option<String>>(2)
            .map_err(|error| storage("read native source successor output", error))?
            .is_none()
        {
            return Err(ProjectionStoreError::ProvenanceCollision);
        }
        affected.push((
            row.get::<String>(0)
                .map_err(|error| storage("read native source predecessor outputs", error))?,
            row.get::<String>(1)
                .map_err(|error| storage("read native source predecessor outputs", error))?,
        ));
    }
    drop(rows);
    // Transfer ownership before deleting the predecessor; output identity remains stable.
    conn.execute(
        &format!(
            "UPDATE {provenance} AS successor SET message_created = 1
         WHERE projector_version = ?1 AND observation_id = ?3{generation_filter}
           AND EXISTS (SELECT 1 FROM {provenance} AS predecessor
             WHERE predecessor.projector_version = successor.projector_version
               AND predecessor.observation_id = ?2 AND predecessor.message_created = 1
               AND predecessor.output_provider = successor.output_provider
               AND predecessor.output_message_id = successor.output_message_id{})",
            if generation.is_some() {
                " AND predecessor.generation = successor.generation"
            } else {
                ""
            },
        ),
        params![SESSION_MESSAGE_PROJECTOR_VERSION, old, new, generation],
    )
    .await
    .map_err(|error| storage("transfer native source output ownership", error))?;
    let aliases = if generation.is_some() {
        "observation_projection_rebuild_aliases"
    } else {
        "observation_projection_aliases"
    };
    let generation_column = if generation.is_some() {
        ", generation"
    } else {
        ""
    };
    conn.execute(&format!(
        "INSERT INTO {aliases} (projector_version, observation_id, output_provider, output_message_id{generation_column})
         SELECT projector_version, ?3, output_provider, output_message_id{generation_column}
         FROM {aliases} WHERE projector_version = ?1 AND observation_id = ?2{generation_filter}
         ON CONFLICT DO NOTHING"),
        params![SESSION_MESSAGE_PROJECTOR_VERSION, old, new, generation],
    ).await.map_err(|error| storage("transfer native source output alias", error))?;
    for (table, version) in [
        (provenance, SESSION_MESSAGE_PROJECTOR_VERSION),
        (workflow, SESSION_MESSAGE_PROJECTOR_VERSION),
        (aliases, SESSION_MESSAGE_PROJECTOR_VERSION),
    ] {
        conn.execute(&format!("DELETE FROM {table} WHERE projector_version = ?1 AND observation_id = ?2 AND ?3 IS NOT NULL{generation_filter}"),
            params![version, old, new, generation]).await.map_err(|error| storage("remove superseded native source effects", error))?;
    }
    let (columns, values, conflict) = if generation.is_some() {
        (
            "projector_version, observation_id, receipt_id, reason, generation",
            "?1, ?2, ?3, ?4, ?5",
            "projector_version, generation, observation_id",
        )
    } else {
        (
            "projector_version, observation_id, receipt_id, reason",
            "?1, ?2, ?3, ?4",
            "projector_version, observation_id",
        )
    };
    let sql = format!(
        "INSERT INTO {dispositions} ({columns}) VALUES ({values})
        ON CONFLICT({conflict}) DO UPDATE SET reason = excluded.reason
        WHERE {dispositions}.receipt_id = excluded.receipt_id"
    );
    let changed = if let Some(generation) = generation {
        conn.execute(
            &sql,
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                old,
                predecessor.receipt().receipt().receipt_id().as_str(),
                ProjectionSkipReason::NativeSourceSuperseded.as_str(),
                generation
            ],
        )
        .await
        .map_err(|error| storage("record native source supersession", error))?
    } else {
        conn.execute(
            &sql,
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                old,
                predecessor.receipt().receipt().receipt_id().as_str(),
                ProjectionSkipReason::NativeSourceSuperseded.as_str()
            ],
        )
        .await
        .map_err(|error| storage("record native source supersession", error))?
    };
    if changed != 1 {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    if generation.is_none() {
        promote_native_source_anchor(conn, &predecessor, successor).await?;
        for (provider, message) in affected {
            reaggregate_output_state_for_output(conn, &provider, &message).await?;
        }
        Box::pin(verify_native_source_supersession(conn, &predecessor)).await?;
    }
    Ok(())
}

async fn observation_anchor(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<RetrievalAnchorRecordV2> {
    let mut rows = conn
        .query(
            "SELECT anchor.anchor_json FROM observation_retrieval_anchors AS binding
         JOIN retrieval_anchors AS anchor ON anchor.anchor_id = binding.anchor_id
         WHERE binding.observation_id = ?1",
            params![observation.observation_id().as_str()],
        )
        .await
        .map_err(|error| storage("read native source anchor", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("read native source anchor", error))?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    let json: String = row
        .get(0)
        .map_err(|error| storage("read native source anchor", error))?;
    let anchor: RetrievalAnchorRecordV2 = serde_json::from_str(&json)
        .map_err(|error| storage("decode native source anchor", error))?;
    anchor.validate()?;
    if anchor.target()
        != &RetrievalAnchorTargetV2::ExactObservation(observation.observation_id().clone())
    {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    Ok(anchor)
}

async fn read_transition_anchors(
    conn: &impl QueryExecutor,
    predecessor: &DurableObservationV1,
    successor: &DurableObservationV1,
) -> ProjectionStoreResult<(RetrievalAnchorRecordV2, RetrievalAnchorRecordV2)> {
    let old = observation_anchor(conn, predecessor).await?;
    let new = observation_anchor(conn, successor).await?;
    let old_auth = old.authorization();
    let new_auth = new.authorization();
    if old.owner() != new.owner()
        || old.aliases() != new.aliases()
        || old_auth.resolved_scope_id != new_auth.resolved_scope_id
        || old_auth.privacy_domain_id != new_auth.privacy_domain_id
        || old_auth.access_policy_digest != new_auth.access_policy_digest
        || old_auth.capability_id != new_auth.capability_id
        || old.payload_access() != new.payload_access()
        || old.retention_class() != new.retention_class()
        || old.durability() != new.durability()
    {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    Ok((old, new))
}

async fn promote_native_source_anchor(
    conn: &impl Executor,
    predecessor: &DurableObservationV1,
    successor: &DurableObservationV1,
) -> ProjectionStoreResult<()> {
    let (old, new) = read_transition_anchors(conn, predecessor, successor).await?;
    let owner = serde_json::to_string(old.owner())
        .map_err(|error| storage("encode native source anchor owner", error))?;
    let disposition = RetrievalAnchorDispositionRecordV1::new(
        format!(
            "native-source:{}:{}",
            old.anchor_id().as_str(),
            new.anchor_id().as_str()
        ),
        old.anchor_id().clone(),
        FactOwnerV1::from(old.owner().clone()),
        AnchorDispositionStateV1::Superseded,
        Some(new.anchor_id().clone()),
        AnchorDispositionReasonClassV1::Correction,
        new.ingested_at(),
    )
    .map_err(|error| storage("construct native source supersession", error))?;
    tracedecay_runtime_core::db::append_retrieval_anchor_disposition_on(conn, &disposition)
        .await
        .map_err(|error| storage("publish native source supersession", error))?;
    for alias in old.aliases() {
        let kind = serde_json::to_string(&alias.kind())
            .map_err(|error| storage("encode native source alias", error))?;
        let locator = serde_json::to_string(alias.locator_digest())
            .map_err(|error| storage("encode native source alias", error))?;
        let mut aliases = conn.query(
            "SELECT anchor_id FROM retrieval_anchor_aliases WHERE owner_json = ?1 AND alias_kind = ?2 AND locator_digest = ?3",
            params![owner.as_str(), kind.as_str(), locator.as_str()],
        ).await.map_err(|error| storage("read native source current alias", error))?;
        let current: String = aliases
            .next()
            .await
            .map_err(|error| storage("read native source current alias", error))?
            .ok_or(ProjectionStoreError::ProvenanceCollision)?
            .get(0)
            .map_err(|error| storage("read native source current alias", error))?;
        drop(aliases);
        if current == new.anchor_id().as_str() {
            continue;
        }
        if current != old.anchor_id().as_str() {
            return Err(ProjectionStoreError::ProvenanceCollision);
        }
        let changed = conn
            .execute(
                "UPDATE retrieval_anchor_aliases SET anchor_id = ?1
             WHERE owner_json = ?2 AND alias_kind = ?3 AND locator_digest = ?4
               AND anchor_id = ?5",
                params![
                    new.anchor_id().as_str(),
                    owner.as_str(),
                    kind,
                    locator,
                    old.anchor_id().as_str()
                ],
            )
            .await
            .map_err(|error| storage("promote native source alias", error))?;
        if changed != 1 {
            return Err(ProjectionStoreError::ProvenanceCollision);
        }
    }
    Ok(())
}

/// Activation has already replaced staged messages and provenance. Publish the
/// predecessor disposition and current alias in that same activation transaction.
pub(super) async fn activate_native_source_transitions(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    let mut after_sequence = 0_i64;
    loop {
        let mut rows = conn.query(
            "SELECT observation.sequence, observation.observation_json
             FROM observation_projection_rebuild_dispositions AS disposition
             JOIN observations AS observation ON observation.observation_id = disposition.observation_id
             WHERE disposition.projector_version = ?1 AND disposition.generation = ?2
               AND disposition.reason = ?3 AND observation.sequence > ?4
             ORDER BY observation.sequence LIMIT ?5",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, generation,
                ProjectionSkipReason::NativeSourceSuperseded.as_str(), after_sequence,
                super::rebuild::REBUILD_PAGE_SIZE],
        ).await.map_err(|error| storage("read staged native source transitions", error))?;
        let mut predecessors = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read staged native source transitions", error))?
        {
            after_sequence = row
                .get(0)
                .map_err(|error| storage("read staged native source sequence", error))?;
            let json: String = row
                .get(1)
                .map_err(|error| storage("read staged native source transitions", error))?;
            predecessors.push(
                serde_json::from_str::<DurableObservationV1>(&json)
                    .map_err(|error| storage("decode staged native source transition", error))?,
            );
        }
        drop(rows);
        if predecessors.is_empty() {
            return Ok(());
        }
        for predecessor in predecessors {
            let successor = read_native_source_successor(conn, &predecessor)
                .await?
                .ok_or(ProjectionStoreError::ProvenanceCollision)?;
            settle_native_source_transition(conn, &successor, SourceTransitionTarget::Live).await?;
        }
    }
}
