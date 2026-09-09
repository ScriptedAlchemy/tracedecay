use std::collections::BTreeSet;

use tempfile::TempDir;
use tracedecay::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
    CanonicalObservationRelationsV1, ClineTranscriptStream, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProviderId, ProviderUsageContractDimensionV1, SessionId,
};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_store::{ObservationProjectionStore, ObservationStore, ProjectionSkipReason};

use super::{
    canonical_observation, canonical_write, canonical_write_with_cursor, isolated_lcm_db_path,
    profile_runtime, projected_raw_store_ids_for_provider, rebuild_projection_to_completion,
    receipt, table_count,
};

/// Consolidation alias over the canonical Cline fixture's unaliased message
/// (`record.projection-cline.0`): the authority audit admits only
/// `consolidated/<lineage>/<unaliased message id>` bindings.
const RETAINED_CLINE_ALIAS: &str = "consolidated/retained/record.projection-cline.0";

fn native_successor(
    old: &DurableObservationV1,
    stream: ClineTranscriptStream,
) -> DurableObservationV1 {
    let mut payload = old.payload().clone();
    let range = if stream == ClineTranscriptStream::UiMessages {
        let evidence = payload.get_mut("evidence").unwrap();
        evidence["range"] = serde_json::json!({"start": 0, "end": 1});
        evidence["native_sequence"] = serde_json::json!(0);
        ObservationSourceRangeV1::new(0, 1).unwrap()
    } else {
        old.identity().position()
    };
    let identity = ObservationIdentityMaterialV1::for_native_record(
        stream
            .source_identity(
                old.source().provider().clone(),
                old.source().session_id().clone(),
            )
            .unwrap(),
        old.scope().clone(),
        old.identity().generation(),
        range,
        old.identity().ordering_domain(),
        old.identity().native_record_id().unwrap().clone(),
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        receipt(
            &format!("receipt.native.{}", old.observation_id().as_str()),
            &payload,
        ),
        old.retention_class().clone(),
        payload,
    )
    .unwrap()
}

fn old_usage() -> DurableObservationV1 {
    let base = canonical_observation("cline", 0);
    let provider = ProviderId::new("cline").unwrap();
    let session = SessionId::new("session.projection-cline").unwrap();
    let native = ObservationId::new("cline.usage-record").unwrap();
    let range = ObservationSourceRangeV1::new(1, 2).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider.clone(),
        "usage",
        native.clone(),
        CanonicalObservationRelationsV1::new(session.clone()),
        vec![CanonicalObservationFactV1::UncorrelatedUsage {
            input_tokens: Some(99),
            output_tokens: Some(4),
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            native_kind: "cline_usage".to_owned(),
            native_field: "tokensIn".to_owned(),
            missing_dimensions: BTreeSet::from([
                ProviderUsageContractDimensionV1::Model,
                ProviderUsageContractDimensionV1::Scope,
                ProviderUsageContractDimensionV1::CounterSemantics,
                ProviderUsageContractDimensionV1::Correlation,
            ]),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range)
            .with_native_sequence(1),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(provider, session).unwrap(),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            native,
        )
        .unwrap(),
        receipt("receipt.cline.old-usage", &payload),
        base.retention_class().clone(),
        payload,
    )
    .unwrap()
}

#[tokio::test]
async fn native_stream_projection_replaces_retained_effects_and_rebuilds_without_duplicates() {
    for activate_through_rebuild in [false, true] {
        let tmp = TempDir::new().unwrap();
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let old = canonical_observation("cline", 0);
        let usage = old_usage();
        let old_write = canonical_write(old.clone());
        let old_cursor = old_write.next_cursor().clone();
        store.persist_observation(old_write.clone()).await.unwrap();
        // A retained alias is a consolidation output binding, so it must carry
        // the shape the authority audit admits at every reopen:
        // `consolidated/<lineage>/<unaliased message id>`.
        let conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
        conn.execute(
        "INSERT INTO observation_projection_aliases (projector_version, observation_id, output_provider, output_message_id)
         VALUES (?1, ?2, 'cline', ?3)",
        rusqlite::params![
            tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION,
            old.observation_id().as_str(),
            RETAINED_CLINE_ALIAS,
        ],
    ).unwrap();
        drop(conn);
        store
            .project_observation(old.observation_id())
            .await
            .unwrap();
        store
            .persist_observation(canonical_write_with_cursor(usage.clone(), Some(old_cursor)))
            .await
            .unwrap();
        store
            .project_observation(usage.observation_id())
            .await
            .unwrap();
        let raw_ids = projected_raw_store_ids_for_provider(&tmp, "cline").await;
        assert_eq!(raw_ids.len(), 1);
        assert_eq!(raw_ids[0].0, RETAINED_CLINE_ALIAS);
        let original_effect = temporal_effect_receipt(&tmp, old.observation_id().as_str());
        let next = native_successor(&old, ClineTranscriptStream::ApiHistory);
        let next_usage = native_successor(&usage, ClineTranscriptStream::UiMessages);
        for observation in [&next, &next_usage] {
            store
                .persist_observation(canonical_write(observation.clone()))
                .await
                .unwrap();
            if !activate_through_rebuild {
                store
                    .project_observation(observation.observation_id())
                    .await
                    .unwrap();
            }
        }
        if activate_through_rebuild {
            rebuild_projection_to_completion(&store, 4).await;
        }
        assert_eq!(
            projected_raw_store_ids_for_provider(&tmp, "cline").await,
            raw_ids
        );
        assert_eq!(
            table_count(&tmp, "observation_provider_usage").await,
            0,
            "uncorrelated Cline counters must never become billable usage"
        );
        assert_eq!(
            table_count(&tmp, "observation_projection_provenance").await,
            1
        );
        store.persist_observation(old_write).await.unwrap();
        store
            .project_observation(old.observation_id())
            .await
            .unwrap();
        rebuild_projection_to_completion(&store, 4).await;
        assert_eq!(
            projected_raw_store_ids_for_provider(&tmp, "cline").await,
            raw_ids
        );
        assert_eq!(
            table_count(&tmp, "observation_provider_usage").await,
            0,
            "uncorrelated Cline counters must never become billable usage"
        );
        drop(store);
        drop(runtime);
        let reopened = profile_runtime(&tmp).await;
        let store = reopened
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        store
            .project_observation(old.observation_id())
            .await
            .unwrap();
        store
            .project_observation(usage.observation_id())
            .await
            .unwrap();
        assert_eq!(
            table_count(&tmp, "observation_provider_usage").await,
            0,
            "uncorrelated Cline counters must never become billable usage"
        );
        assert_eq!(
            projected_raw_store_ids_for_provider(&tmp, "cline").await,
            raw_ids
        );
        assert_eq!(
            temporal_effect_receipt(&tmp, old.observation_id().as_str()),
            original_effect,
            "source transition, replay and rebuild preserve the immutable temporal effect receipt"
        );
        let conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
        let superseded: i64 = conn.query_row(
            "SELECT COUNT(*) FROM observation_projection_dispositions WHERE reason = 'native_source_superseded'",
            (), |row| row.get(0),
        ).unwrap();
        assert_eq!(
            superseded, 2,
            "both API and UI predecessors remain superseded after reopen and replay"
        );
    }
}

#[tokio::test]
async fn forged_native_source_supersession_requires_a_projected_successor() {
    for capture_successor in [false, true] {
        let tmp = TempDir::new().unwrap();
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let old = canonical_observation("cline", 0);
        store
            .persist_observation(canonical_write(old.clone()))
            .await
            .unwrap();
        store
            .project_observation(old.observation_id())
            .await
            .unwrap();
        if capture_successor {
            store
                .persist_observation(canonical_write(native_successor(
                    &old,
                    ClineTranscriptStream::ApiHistory,
                )))
                .await
                .unwrap();
        }
        drop(store);
        drop(runtime);
        let conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
        conn.execute(
            "DELETE FROM observation_projection_provenance WHERE observation_id = ?1",
            [old.observation_id().as_str()],
        )
        .unwrap();
        conn.execute("INSERT INTO observation_projection_dispositions (projector_version, observation_id, receipt_id, reason) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION, old.observation_id().as_str(),
                old.receipt().receipt().receipt_id().as_str(), ProjectionSkipReason::NativeSourceSuperseded.as_str()]).unwrap();
        drop(conn);
        assert!(
            HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
                .await
                .is_err(),
            "a forged label must fail with either a missing or an unprojected successor"
        );
    }
}

#[tokio::test]
async fn native_source_projection_failure_keeps_predecessor_authority_until_retry() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let old = canonical_observation("cline", 0);
    let old_write = canonical_write(old.clone());
    let old_anchor = old_write.retrieval_anchor_id().as_str().to_owned();
    store.persist_observation(old_write).await.unwrap();
    store
        .project_observation(old.observation_id())
        .await
        .unwrap();
    let raw_ids = projected_raw_store_ids_for_provider(&tmp, "cline").await;
    let next = native_successor(&old, ClineTranscriptStream::ApiHistory);
    let next_write = canonical_write(next.clone());
    let next_anchor = next_write.retrieval_anchor_id().as_str().to_owned();
    store.persist_observation(next_write).await.unwrap();
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_native_source_disposition BEFORE INSERT ON observation_projection_dispositions
        WHEN NEW.reason = 'native_source_superseded'
        BEGIN SELECT RAISE(ABORT, 'fixture native source activation failure'); END;").unwrap();
    assert!(
        store
            .project_observation(next.observation_id())
            .await
            .unwrap_err()
            .to_string()
            .contains("fixture native source activation failure")
    );
    let active_alias: String = conn
        .query_row(
            "SELECT anchor_id FROM retrieval_anchor_aliases",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_alias, old_anchor);
    let owner: String = conn
        .query_row(
            "SELECT observation_id FROM observation_projection_provenance",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owner, old.observation_id().as_str());
    assert_eq!(
        projected_raw_store_ids_for_provider(&tmp, "cline").await,
        raw_ids
    );
    conn.execute_batch("DROP TRIGGER reject_native_source_disposition;")
        .unwrap();
    drop(conn);
    drop(store);
    drop(runtime);
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    store
        .project_observation(next.observation_id())
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    let active_alias: String = conn
        .query_row(
            "SELECT anchor_id FROM retrieval_anchor_aliases",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_alias, next_anchor);
    assert_eq!(
        projected_raw_store_ids_for_provider(&tmp, "cline").await,
        raw_ids
    );
}

#[tokio::test]
async fn native_source_wrong_predecessor_receipt_is_rejected_before_activation() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let old = canonical_observation("cline", 0);
    let old_write = canonical_write(old.clone());
    let old_anchor = old_write.retrieval_anchor_id().as_str().to_owned();
    store.persist_observation(old_write).await.unwrap();
    store
        .project_observation(old.observation_id())
        .await
        .unwrap();
    let raw_ids = projected_raw_store_ids_for_provider(&tmp, "cline").await;
    assert_eq!(raw_ids.len(), 1);
    let next = native_successor(&old, ClineTranscriptStream::ApiHistory);
    let next_write = canonical_write(next.clone());
    let next_anchor = next_write.retrieval_anchor_id().as_str().to_owned();
    store.persist_observation(next_write).await.unwrap();
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    // Both receipts exist, but the canonical guard rejects a foreign binding.
    let error = conn.execute(
        "INSERT INTO observation_projection_dispositions (projector_version, observation_id, receipt_id, reason)
         VALUES (?1, ?2, ?3, 'non_conversational_record')",
        rusqlite::params![tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION,
            old.observation_id().as_str(), next.receipt().receipt().receipt_id().as_str()],
    ).unwrap_err();
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(ref code, ref message)
        if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
            && message.as_deref() == Some("projection disposition receipt mismatch")),
        "{error}"
    );
    let active_alias: String = conn
        .query_row(
            "SELECT anchor_id FROM retrieval_anchor_aliases",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_alias, old_anchor);
    let owner: String = conn
        .query_row(
            "SELECT observation_id FROM observation_projection_provenance",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owner, old.observation_id().as_str());
    assert_eq!(
        table_count(&tmp, "observation_projection_dispositions").await,
        0
    );
    assert_eq!(table_count(&tmp, "retrieval_anchor_dispositions").await, 0);
    assert_eq!(
        projected_raw_store_ids_for_provider(&tmp, "cline").await,
        raw_ids
    );
    drop(conn);
    store
        .project_observation(next.observation_id())
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    let active_alias: String = conn
        .query_row(
            "SELECT anchor_id FROM retrieval_anchor_aliases",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_alias, next_anchor);
    assert_eq!(
        projected_raw_store_ids_for_provider(&tmp, "cline").await,
        raw_ids
    );
}

fn temporal_effect_receipt(tmp: &TempDir, observation_id: &str) -> String {
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    conn.query_row(
        "SELECT json_array(observation_id, observation_sequence, session_id, receipt_id,
                           effect_digest, output_count, recorded_at)
         FROM session_temporal_observation_effects WHERE observation_id = ?1",
        [observation_id],
        |row| row.get(0),
    )
    .unwrap()
}
