//! A record a source rewrite retired projects again only after admission
//! accepts an offer from a later layout of the same source.

use tempfile::TempDir;

use tracedecay_domain::{
    DurableObservationV1, ObservationIdentityMaterialV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    SessionId, UtcMicros,
};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationStoreError, ObservationWrite, ProjectionPersistOutcome,
    ProjectionRebuildOutcome, ProjectionSkipReason, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor,
};

use crate::observation_batch_tests::{sequential_writes, sequential_writes_with_text};
use crate::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, RegisteredGlobalDbTestRuntime,
};
use crate::{GlobalDbObservationStore, ObservationSourcePresenceV1, RegisteredGlobalDb};

/// One file-byte source, laid out the way admission lays out a rewrite.
struct Source<'a> {
    registered: &'a RegisteredGlobalDb,
    identity: ObservationSourceIdentityV1,
    scope: ObservationScopeV1,
}

impl<'a> Source<'a> {
    fn of(registered: &'a RegisteredGlobalDb, writes: &[AnchoredObservationWrite]) -> Self {
        Self {
            registered,
            identity: writes[0].observation().source().clone(),
            scope: writes[0].observation().scope().clone(),
        }
    }

    async fn offer(&self, generation: u64, offered: &[&AnchoredObservationWrite]) {
        let presence: Vec<_> = offered
            .iter()
            .map(|write| ObservationSourcePresenceV1 {
                observation_id: write.observation().observation_id().clone(),
                generation: ObservationSourceGenerationV1::new(generation).unwrap(),
                start_offset: write.observation().identity().position().start(),
            })
            .collect();
        self.registered
            .record_observation_source_presence(&self.identity, &self.scope, &presence)
            .await
            .unwrap();
    }

    async fn accept(&self, generation: u64, write: &AnchoredObservationWrite) {
        let store = self.registered.observation_store();
        let expected = store
            .get_source_cursor(&self.identity, &self.scope)
            .await
            .unwrap();
        let candidate = reoffer(write, generation, expected);
        assert!(matches!(
            store.persist_observation(candidate).await.unwrap(),
            ObservationPersistOutcome::CoveredDuplicate(_)
                | ObservationPersistOutcome::ExactDuplicate(_)
        ));
    }

    /// Rewrites the source into a layout that retains the previous layout's
    /// records before `retained_through` and offers `offered` after it.
    /// Returns how many records the completed rewrite retired.
    async fn rewrite(
        &self,
        previous: u64,
        generation: u64,
        retained_through: u64,
        offered: &[&AnchoredObservationWrite],
    ) -> u64 {
        self.registered
            .begin_observation_source_rewrite(
                &self.identity,
                &self.scope,
                ObservationSourceGenerationV1::new(previous).unwrap(),
                ObservationSourceGenerationV1::new(generation).unwrap(),
                retained_through,
            )
            .await
            .unwrap();
        self.offer(generation, offered).await;
        self.registered
            .complete_observation_source_rewrite(
                &self.identity,
                &self.scope,
                ObservationSourceGenerationV1::new(generation).unwrap(),
            )
            .await
            .unwrap()
    }
}

fn reoffer(
    write: &AnchoredObservationWrite,
    generation: u64,
    expected: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let original = write.observation();
    let identity = original.identity();
    let generation = ObservationSourceGenerationV1::new(generation).unwrap();
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            original.source().clone(),
            original.scope().clone(),
            generation,
            identity.position(),
            identity.ordering_domain(),
            identity.native_record_id().clone(),
        )
        .unwrap(),
        original.receipt().clone(),
        original.retention_class().clone(),
        original.payload().clone(),
    )
    .unwrap();
    let next = ObservationSourceCursorV1::for_ordering(
        original.source().clone(),
        original.scope().clone(),
        generation,
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    let expected = if expected
        .as_ref()
        .is_some_and(|cursor| cursor.reached(&next))
    {
        write.expected_cursor().cloned()
    } else {
        expected
    };
    let anchor = build_observation_retrieval_anchor(
        &observation,
        write.projection_generation().clone(),
        UtcMicros(1),
        build_observation_resolution_authorization_v1(&observation, "observation-batch-test")
            .unwrap(),
    )
    .unwrap();
    AnchoredObservationWrite::new(
        ObservationWrite::new(observation, expected, next).unwrap(),
        anchor,
        write.projection_generation().clone(),
    )
    .unwrap()
}

async fn persist(store: &GlobalDbObservationStore, writes: &[AnchoredObservationWrite]) {
    for chunk in writes.chunks(100) {
        store.persist_observations(chunk.to_vec()).await.unwrap();
    }
}

async fn drain(store: &GlobalDbObservationStore) -> Vec<ProjectionPersistOutcome> {
    let mut outcomes = Vec::new();
    loop {
        let batch = store
            .project_queued_observations(128)
            .await
            .unwrap()
            .unwrap();
        outcomes.extend(batch.items.into_iter().map(|item| item.outcome));
        if !batch.has_more {
            return outcomes;
        }
    }
}

async fn rebuild(registered: &RegisteredGlobalDb) -> ProjectionRebuildOutcome {
    let snapshot = registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query("SELECT MAX(sequence) FROM observations", ())
        .await
        .unwrap();
    let frontier = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    drop(rows);
    drop(snapshot);
    registered
        .rebuild_observation_projection(u64::try_from(frontier).unwrap())
        .await
        .unwrap()
}

async fn rebuild_to_completion(registered: &RegisteredGlobalDb) -> ProjectionRebuildOutcome {
    loop {
        let outcome = rebuild(registered).await;
        if outcome.is_complete() {
            return outcome;
        }
    }
}

async fn projected_messages(registered: &RegisteredGlobalDb) -> Vec<String> {
    let snapshot = registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT message_id FROM lcm_raw_messages ORDER BY message_id",
            (),
        )
        .await
        .unwrap();
    let mut messages = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        messages.push(row.get::<String>(0).unwrap());
    }
    messages
}

async fn projected_message_content(registered: &RegisteredGlobalDb, message_id: &str) -> String {
    let snapshot = registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COALESCE(content, placeholder_text, '')
             FROM lcm_raw_messages WHERE message_id = ?1",
            params![message_id],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap()
}

async fn temporal_effect(
    registered: &RegisteredGlobalDb,
    write: &AnchoredObservationWrite,
) -> (String, i64) {
    let snapshot = registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT effect_digest, output_count
             FROM session_temporal_observation_effects WHERE observation_id = ?1",
            params![write.observation().observation_id().as_str()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    (row.get::<String>(0).unwrap(), row.get::<i64>(1).unwrap())
}

async fn projection_disposition(
    registered: &RegisteredGlobalDb,
    write: &AnchoredObservationWrite,
) -> String {
    let snapshot = registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT reason FROM observation_projection_dispositions
             WHERE observation_id = ?1",
            params![write.observation().observation_id().as_str()],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap()
}

async fn temporal_reset_requested(
    registered: &RegisteredGlobalDb,
    write: &AnchoredObservationWrite,
) -> bool {
    let snapshot = registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT EXISTS (
                 SELECT 1
                 FROM session_temporal_resets AS reset
                 JOIN session_temporal_observation_effects AS effect
                   ON effect.session_id = reset.session_id
                 WHERE effect.observation_id = ?1
             )",
            params![write.observation().observation_id().as_str()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap() == 1
}

async fn source_presence_generation(
    registered: &RegisteredGlobalDb,
    write: &AnchoredObservationWrite,
) -> u64 {
    let snapshot = registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT generation FROM observation_source_presence
             WHERE observation_id = ?1",
            params![write.observation().observation_id().as_str()],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap()
        .parse()
        .unwrap()
}

async fn clear_temporal_reset(registered: &RegisteredGlobalDb, write: &AnchoredObservationWrite) {
    registered
        .writer_connection()
        .unwrap()
        .execute(
            "DELETE FROM session_temporal_resets
             WHERE session_id = (
                 SELECT session_id FROM session_temporal_observation_effects
                 WHERE observation_id = ?1
             )",
            params![write.observation().observation_id().as_str()],
        )
        .await
        .unwrap();
}

#[asupersync::test]
async fn a_reoffered_retired_record_projects_again_across_rebuild_and_restart() {
    let tmp = TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(tmp.path())
        .await
        .unwrap();
    let writes = sequential_writes(&SessionId::new("session.source-reoffer").unwrap(), 2);
    {
        let registered = runtime.profile_database();
        let store = registered.observation_store();
        let source = Source::of(registered, &writes);
        source.offer(1, &[&writes[0], &writes[1]]).await;
        persist(&store, &writes).await;
        drain(&store).await;
        assert_eq!(
            projected_messages(registered).await,
            ["message.batch.0", "message.batch.1"]
        );
        assert_eq!(source.rewrite(1, 2, 0, &[]).await, 2);
        assert!(projected_messages(registered).await.is_empty());

        assert!(matches!(
            store.persist_observation(writes[0].clone()).await.unwrap(),
            ObservationPersistOutcome::ExactDuplicate(_)
        ));
        drain(&store).await;
        assert!(
            projected_messages(registered).await.is_empty(),
            "a replayed durable write is not an offer from its source"
        );
        rebuild_to_completion(registered).await;
        assert!(
            projected_messages(registered).await.is_empty(),
            "a rebuild keeps the retirement"
        );
    }

    let runtime = runtime.reopen_profile_database_for_test().await.unwrap();
    {
        let registered = runtime.profile_database();
        let source = Source::of(registered, &writes);
        assert!(projected_messages(registered).await.is_empty());
        assert_eq!(source.rewrite(2, 3, 0, &[&writes[0]]).await, 0);
        assert!(
            projected_messages(registered).await.is_empty(),
            "presence is not acceptance"
        );
        let store = registered.observation_store();
        store.persist_observation(writes[0].clone()).await.unwrap();
        drain(&store).await;
        assert!(
            projected_messages(registered).await.is_empty(),
            "old coordinates do not accept the current offer"
        );
        source.accept(3, &writes[0]).await;
        assert!(
            projected_messages(registered).await.is_empty(),
            "projection resumes after acceptance"
        );
    }

    let runtime = runtime.reopen_profile_database_for_test().await.unwrap();
    {
        let registered = runtime.profile_database();
        let source = Source::of(registered, &writes);
        drain(&registered.observation_store()).await;
        assert_eq!(
            projected_messages(registered).await,
            ["message.batch.0"],
            "the re-offered record projects again; the other stays retired"
        );
        source.offer(3, &[&writes[0]]).await;
        assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
        rebuild_to_completion(registered).await;
        assert_eq!(
            projected_messages(registered).await,
            ["message.batch.0"],
            "a rebuild keeps the revival"
        );
    }

    let runtime = runtime.reopen_profile_database_for_test().await.unwrap();
    let registered = runtime.profile_database();
    assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
}

#[asupersync::test]
async fn a_newer_layout_retires_a_revived_record_again() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let writes = sequential_writes(&SessionId::new("session.source-relayout").unwrap(), 1);
    let source = Source::of(registered, &writes);
    source.offer(1, &[&writes[0]]).await;
    persist(&store, &writes).await;
    drain(&store).await;

    assert_eq!(source.rewrite(1, 2, 0, &[]).await, 1);
    assert_eq!(source.rewrite(2, 3, 0, &[&writes[0]]).await, 0);
    source.accept(3, &writes[0]).await;
    drain(&store).await;
    assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
    assert_eq!(source.rewrite(3, 4, 0, &[]).await, 1);
    assert!(projected_messages(registered).await.is_empty());
    assert_eq!(source.rewrite(4, 5, 0, &[&writes[0]]).await, 0);
    source.accept(5, &writes[0]).await;
    drain(&store).await;
    assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
}

#[asupersync::test]
async fn a_reoffered_record_settles_if_another_source_claimed_its_retired_output() {
    let tmp = TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(tmp.path())
        .await
        .unwrap();
    let original = sequential_writes_with_text(
        &SessionId::new("session.source-collision.original").unwrap(),
        1,
        |_| "original source body".to_owned(),
    );
    let claimant = sequential_writes_with_text(
        &SessionId::new("session.source-collision.claimant").unwrap(),
        1,
        |_| "claimant source body".to_owned(),
    );

    {
        let registered = runtime.profile_database();
        let store = registered.observation_store();
        let original_source = Source::of(registered, &original);
        original_source.offer(1, &[&original[0]]).await;
        persist(&store, &original).await;
        drain(&store).await;
        let original_effect = temporal_effect(registered, &original[0]).await;
        assert_eq!(original_effect.1, 1);

        assert_eq!(original_source.rewrite(1, 2, 0, &[]).await, 1);
        let claimant_source = Source::of(registered, &claimant);
        claimant_source.offer(1, &[&claimant[0]]).await;
        persist(&store, &claimant).await;
        drain(&store).await;
        assert_eq!(
            projected_message_content(registered, "message.batch.0").await,
            "claimant source body"
        );
        clear_temporal_reset(registered, &original[0]).await;
        assert!(!temporal_reset_requested(registered, &original[0]).await);

        assert_eq!(
            original_source.rewrite(2, 3, 0, &[&original[0]]).await,
            0,
            "the re-offer settles as a live collision instead of rolling back"
        );
        original_source.accept(3, &original[0]).await;
        drain(&store).await;
        assert_eq!(
            projection_disposition(registered, &original[0]).await,
            ProjectionSkipReason::OutputCollision.as_str()
        );
        assert_eq!(
            temporal_effect(registered, &original[0]).await,
            original_effect,
            "the original positive-output temporal receipt stays immutable"
        );
        assert!(
            temporal_reset_requested(registered, &original[0]).await,
            "the retained positive receipt still requests a live-state reset"
        );
        assert_eq!(
            projected_message_content(registered, "message.batch.0").await,
            "claimant source body",
            "the collided re-offer must not reclaim the live output"
        );
        original_source.offer(3, &[&original[0]]).await;
        assert_eq!(
            source_presence_generation(registered, &original[0]).await,
            3
        );
    }

    let runtime = runtime.reopen_profile_database_for_test().await.unwrap();
    let registered = runtime.profile_database();
    let original_source = Source::of(registered, &original);
    original_source.offer(3, &[&original[0]]).await;
    assert_eq!(
        source_presence_generation(registered, &original[0]).await,
        3
    );
    assert_eq!(
        projection_disposition(registered, &original[0]).await,
        ProjectionSkipReason::OutputCollision.as_str()
    );
    assert_eq!(
        projected_message_content(registered, "message.batch.0").await,
        "claimant source body"
    );
}

#[asupersync::test]
async fn a_foreign_source_cannot_revive_a_retired_record() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let store = registered.observation_store();
    let writes = sequential_writes(&SessionId::new("session.source-owner").unwrap(), 1);
    let source = Source::of(registered, &writes);
    source.offer(1, &[&writes[0]]).await;
    persist(&store, &writes).await;
    drain(&store).await;
    assert_eq!(source.rewrite(1, 2, 0, &[]).await, 1);

    let foreign =
        ObservationSourceIdentityV1::new(SessionId::new("session.source-foreign").unwrap())
            .unwrap();
    registered
        .record_observation_source_presence(
            &foreign,
            &source.scope,
            &[ObservationSourcePresenceV1 {
                observation_id: writes[0].observation().observation_id().clone(),
                generation: ObservationSourceGenerationV1::new(3).unwrap(),
                start_offset: 0,
            }],
        )
        .await
        .unwrap();
    store.persist_observation(writes[0].clone()).await.unwrap();
    drain(&store).await;
    assert!(projected_messages(registered).await.is_empty());
    assert_eq!(source.rewrite(2, 3, 0, &[&writes[0]]).await, 0);
    source.accept(3, &writes[0]).await;
    drain(&store).await;
    assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
}

#[asupersync::test]
async fn a_failed_presence_batch_does_not_authorize_revival() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let store = registered.observation_store();
    let writes = sequential_writes(&SessionId::new("session.source-rollback").unwrap(), 2);
    let source = Source::of(registered, &writes);
    source.offer(1, &[&writes[0], &writes[1]]).await;
    persist(&store, &writes).await;
    drain(&store).await;
    assert_eq!(source.rewrite(1, 2, 0, &[]).await, 2);

    let generation = ObservationSourceGenerationV1::new(3).unwrap();
    let error = registered
        .record_observation_source_presence(
            &source.identity,
            &source.scope,
            &[
                ObservationSourcePresenceV1 {
                    observation_id: writes[0].observation().observation_id().clone(),
                    generation,
                    start_offset: 0,
                },
                ObservationSourcePresenceV1 {
                    observation_id: writes[1].observation().observation_id().clone(),
                    generation,
                    start_offset: u64::MAX,
                },
            ],
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        tracedecay_store::ProjectionStoreError::SequenceOverflow(u64::MAX)
    ));
    assert!(projected_messages(registered).await.is_empty());
    rebuild_to_completion(registered).await;
    assert!(projected_messages(registered).await.is_empty());
    assert_eq!(source.rewrite(2, 3, 0, &[&writes[0]]).await, 0);
    source.accept(3, &writes[0]).await;
    drain(&store).await;
    assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
}

#[asupersync::test]
async fn a_record_retired_before_its_drain_projects_once_offered_again() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let writes = sequential_writes(&SessionId::new("session.source-queued").unwrap(), 2);
    let source = Source::of(registered, &writes);
    source.offer(1, &[&writes[0], &writes[1]]).await;
    persist(&store, &writes).await;

    assert_eq!(source.rewrite(1, 2, 0, &[]).await, 2);
    assert_eq!(source.rewrite(2, 3, 0, &[&writes[0]]).await, 0);
    source.accept(3, &writes[0]).await;
    let outcomes = drain(&store).await;
    assert!(
        matches!(
            outcomes.as_slice(),
            [
                ProjectionPersistOutcome::Projected(_),
                ProjectionPersistOutcome::Skipped {
                    reason: ProjectionSkipReason::SourceRecordRetired,
                    ..
                },
            ]
        ),
        "{outcomes:?}"
    );
    assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
}

#[asupersync::test]
async fn a_rebuild_that_staged_a_retirement_activates_the_revival() {
    // Three alias pages and one build page fill the first rebuild call's
    // step budget, so it stages the retirement and stops before activation.
    const RECORDS: usize = 300;

    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let writes = sequential_writes(&SessionId::new("session.source-rebuild").unwrap(), RECORDS);
    let all: Vec<_> = writes.iter().collect();
    let source = Source::of(registered, &writes);
    source.offer(1, &all).await;
    persist(&store, &writes).await;
    drain(&store).await;
    assert_eq!(source.rewrite(1, 2, 0, &all[1..]).await, 1);

    let staged = rebuild(registered).await;
    assert!(!staged.is_complete());
    assert_eq!(staged.skipped_observations(), 1, "the retirement is staged");

    let retained_through = u64::try_from(RECORDS).unwrap();
    assert_eq!(
        source.rewrite(2, 3, retained_through, &[&writes[0]]).await,
        0
    );
    source.accept(3, &writes[0]).await;
    let rebuilt = rebuild_to_completion(registered).await;
    assert_eq!(rebuilt.skipped_observations(), 0);
    let messages = projected_messages(registered).await;
    assert_eq!(messages.len(), RECORDS);
    assert!(messages.iter().any(|message| message == "message.batch.0"));
}

#[asupersync::test]
async fn a_rebuild_does_not_restore_a_revived_record_retired_again() {
    let tmp = TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(tmp.path())
        .await
        .unwrap();
    let registered = runtime.profile_database();
    let store = registered.observation_store();
    let writes = sequential_writes(
        &SessionId::new("session.source-retired-again").unwrap(),
        300,
    );
    let all: Vec<_> = writes.iter().collect();
    let source = Source::of(registered, &writes);
    source.offer(1, &all).await;
    persist(&store, &writes).await;
    drain(&store).await;
    assert_eq!(source.rewrite(1, 2, 0, &all[1..]).await, 1);
    let staged = rebuild(registered).await;
    assert!(!staged.is_complete());
    assert_eq!(staged.skipped_observations(), 1);
    source.rewrite(2, 3, 300, &[&writes[0]]).await;
    source.accept(3, &writes[0]).await;
    let first = store
        .project_observation(writes[0].observation().observation_id())
        .await
        .unwrap();
    assert!(matches!(first, ProjectionPersistOutcome::ExactDuplicate(_)));
    assert_eq!(source.rewrite(3, 4, 0, &all[1..]).await, 1);
    assert!(
        !projected_messages(registered)
            .await
            .contains(&"message.batch.0".to_owned())
    );

    let rebuilt = rebuild_to_completion(registered).await;
    assert!(
        !projected_messages(registered)
            .await
            .contains(&"message.batch.0".to_owned()),
        "activation must preserve a retirement after staging its revival"
    );
    assert_eq!(rebuilt.skipped_observations(), 1);
}

#[asupersync::test]
async fn retirement_cancels_pending_revival_and_retained_prefix_preserves_acceptance() {
    let tmp = TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(tmp.path())
        .await
        .unwrap();
    let registered = runtime.profile_database();
    let store = registered.observation_store();
    let writes = sequential_writes(&SessionId::new("session.source-pending").unwrap(), 1);
    let source = Source::of(registered, &writes);
    source.offer(1, &[&writes[0]]).await;
    persist(&store, &writes).await;
    drain(&store).await;
    assert_eq!(source.rewrite(1, 2, 0, &[]).await, 1);
    source.rewrite(2, 3, 0, &[&writes[0]]).await;
    source.accept(3, &writes[0]).await;
    source.rewrite(3, 4, 0, &[&writes[0]]).await;
    drain(&store).await;
    assert!(
        projected_messages(registered).await.is_empty(),
        "acceptance of an older layout does not accept the current offer"
    );
    source.accept(4, &writes[0]).await;
    assert_eq!(source.rewrite(4, 5, 0, &[]).await, 0);
    drain(&store).await;
    assert!(projected_messages(registered).await.is_empty());
    source.rewrite(5, 6, 0, &[&writes[0]]).await;
    drain(&store).await;
    assert!(projected_messages(registered).await.is_empty());

    source.accept(6, &writes[0]).await;
    source.rewrite(6, 7, 1, &[]).await;
    drain(&store).await;
    assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
}

#[asupersync::test]
async fn a_failed_accepted_batch_rolls_back_revival_and_coverage() {
    let tmp = TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(tmp.path())
        .await
        .unwrap();
    let registered = runtime.profile_database();
    let store = registered.observation_store();
    let writes = sequential_writes(
        &SessionId::new("session.source-accept-rollback").unwrap(),
        2,
    );
    let source = Source::of(registered, &writes);
    source.offer(1, &[&writes[0], &writes[1]]).await;
    persist(&store, &writes).await;
    drain(&store).await;
    assert_eq!(source.rewrite(1, 2, 0, &[]).await, 2);
    source.rewrite(2, 3, 0, &[&writes[0], &writes[1]]).await;
    let previous = store
        .get_source_cursor(&source.identity, &source.scope)
        .await
        .unwrap();
    let first = reoffer(&writes[0], 3, previous.clone());
    let second = reoffer(&writes[1], 3, Some(first.next_cursor().clone()));
    registered
        .writer_connection()
        .unwrap()
        .execute(
            "CREATE TRIGGER fail_second_accepted_offer
         BEFORE INSERT ON projection_queue
         WHEN NEW.observation_id = (SELECT observation_id FROM observations WHERE sequence = 2)
         BEGIN SELECT RAISE(ABORT, 'injected accepted-offer write failure'); END",
            (),
        )
        .await
        .unwrap();
    let error = store
        .persist_observations(vec![first.clone(), second.clone()])
        .await
        .unwrap_err();
    let ObservationStoreError::Storage {
        operation,
        source: cause,
    } = error
    else {
        panic!("expected the injected storage failure, got {error:?}");
    };
    assert_eq!(operation, "submit observation batch");
    assert!(
        cause
            .to_string()
            .contains("injected accepted-offer write failure"),
        "{cause}"
    );
    registered
        .writer_connection()
        .unwrap()
        .execute("DROP TRIGGER fail_second_accepted_offer", ())
        .await
        .unwrap();
    assert_eq!(
        store
            .get_source_cursor(&source.identity, &source.scope)
            .await
            .unwrap(),
        previous
    );
    drain(&store).await;
    assert!(
        projected_messages(registered).await.is_empty(),
        "the first offer rolled back with the second"
    );

    let outcomes = store
        .persist_observations(vec![first, second])
        .await
        .unwrap();
    assert!(outcomes.iter().all(|item| matches!(
        item.outcome(),
        ObservationPersistOutcome::CoveredDuplicate(_)
    )));
    registered
        .writer_connection()
        .unwrap()
        .execute(
            "CREATE TRIGGER fail_revival_reset
             BEFORE INSERT ON session_temporal_resets
             BEGIN SELECT RAISE(ABORT, 'injected revival projection failure'); END",
            (),
        )
        .await
        .unwrap();
    let error = store.project_queued_observations(128).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected revival projection failure"),
        "{error:?}"
    );
    assert!(
        projected_messages(registered).await.is_empty(),
        "the failed projector must roll back restored outputs"
    );
    assert_eq!(
        projection_disposition(registered, &writes[0]).await,
        ProjectionSkipReason::SourceRecordRetired.as_str()
    );
    registered
        .writer_connection()
        .unwrap()
        .execute("DROP TRIGGER fail_revival_reset", ())
        .await
        .unwrap();
    drain(&store).await;
    assert_eq!(
        projected_messages(registered).await,
        ["message.batch.0", "message.batch.1"]
    );
}

#[asupersync::test]
async fn an_exact_accepted_reoffer_revives_without_a_transport_receipt() {
    let tmp = TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(tmp.path())
        .await
        .unwrap();
    let writes = sequential_writes(&SessionId::new("session.source-exact-reoffer").unwrap(), 1);
    {
        let registered = runtime.profile_database();
        let store = registered.observation_store();
        let source = Source::of(registered, &writes);
        source.offer(1, &[&writes[0]]).await;
        persist(&store, &writes).await;
        drain(&store).await;
        assert_eq!(source.rewrite(1, 2, 0, &[]).await, 1);
        source.rewrite(2, 1, 0, &[&writes[0]]).await;
        let outcome = store.persist_observation(writes[0].clone()).await.unwrap();
        assert!(matches!(
            outcome,
            ObservationPersistOutcome::ExactDuplicate(_)
        ));
        let snapshot = registered.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query("SELECT COUNT(*) FROM td_runtime_writer_idempotency_v2", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }
    let runtime = runtime.reopen_profile_database_for_test().await.unwrap();
    let registered = runtime.profile_database();
    drain(&registered.observation_store()).await;
    assert_eq!(projected_messages(registered).await, ["message.batch.0"]);
}
