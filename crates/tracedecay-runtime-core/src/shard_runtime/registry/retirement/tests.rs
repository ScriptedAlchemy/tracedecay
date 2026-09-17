use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_domain::{BrainId, ProjectId, UserProfileId, UtcMicros};
use tracedecay_store::{
    RuntimeLeaseIdV1, RuntimeLeaseV1, RuntimeMaintenanceStateV1, RuntimePublicationIdV1,
    StoreAuthorityEpochV1, StoreClientIdV1, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreRuntimeRegistryPublicationV1, StoreShardIdV1, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

use super::*;
use crate::shard_runtime::registry::{
    EmptyPhysicalRuntimeAttachment, PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot,
    ResolvedStoreLocator, ShardRuntimeBuildRequest, ShardRuntimePublisher, StoreRuntimeLookup,
    StoreRuntimeOpenBegin, StoreRuntimeOpenMode, StoreRuntimeOpenRequest,
    StoreRuntimeRegistryFuture, StoreRuntimeResolver,
};
use crate::shard_runtime::shard::ShardRuntime;

struct UnusedResolver;

impl StoreRuntimeResolver for UnusedResolver {
    fn resolve<'a>(
        &'a self,
        _key: &'a StoreRuntimeKey,
        _mode: StoreRuntimeOpenMode,
        _database_authority: Option<&'a DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async {
            Err(StoreRuntimeRegistryFailure::ResolverFailed {
                message: "retirement fixture never opens a runtime".to_owned(),
            })
        })
    }

    fn resolve_graph<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        // The locator digest requires `Path::is_absolute`, which is
        // host-specific: a bare `/...` literal is not absolute on Windows.
        let path = std::path::PathBuf::from(format!(
            "{}retirement-graph/{:?}/{}",
            if cfg!(windows) { "C:\\" } else { "/" },
            key.shard_id.scope,
            key.incarnation.get()
        ));
        let locator = VerifiedStoreLocatorV1::new(
            key.shard_id.clone(),
            key.incarnation,
            canonical_store_locator_digest(&path).unwrap(),
        );
        Box::pin(async move { Ok(ResolvedStoreLocator::new(locator, path)) })
    }
}

struct UnusedPublisher;

impl ShardRuntimePublisher for UnusedPublisher {
    fn publish(
        &self,
        _request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<
        '_,
        Result<super::super::PublishedShardRuntime, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async {
            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "publish retirement fixture runtime",
                message: "retirement fixture installs a ready owner directly".to_owned(),
            })
        })
    }
}

struct DrainFailure;

impl PhysicalRuntimeAttachment for DrainFailure {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot {
        PhysicalRuntimeSnapshot::default()
    }

    fn opened_file_identity(&self) -> Result<u64, String> {
        Ok(0)
    }

    fn drain(&self) -> Result<(), String> {
        Err("injected retirement drain failure".to_owned())
    }

    fn close_and_join(&self) -> Result<(), String> {
        Ok(())
    }
}

struct CloseFailure;

impl PhysicalRuntimeAttachment for CloseFailure {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot {
        PhysicalRuntimeSnapshot::default()
    }

    fn opened_file_identity(&self) -> Result<u64, String> {
        Ok(0)
    }

    fn drain(&self) -> Result<(), String> {
        Ok(())
    }

    fn close_and_join(&self) -> Result<(), String> {
        Err("injected irreversible close failure".to_owned())
    }
}

struct FailingOwnerAttachmentReservation {
    identity: super::super::DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
    _attachment: super::super::DatabaseRuntimeAttachment,
    fail_preflight: bool,
    fail_commit: bool,
    terminalized: Arc<AtomicBool>,
}

impl Drop for FailingOwnerAttachmentReservation {
    fn drop(&mut self) {
        if !self.terminalized.load(Ordering::SeqCst) {
            let _ = self.identity.restore();
        }
    }
}

impl StoreRuntimeOwnerAttachmentRetirementReservationV1 for FailingOwnerAttachmentReservation {
    fn identity(&self) -> &super::super::DatabaseRuntimeOwnerAttachmentReservationIdentityV1 {
        &self.identity
    }

    fn try_into_database_owner_retirement_reservation(
        self: Box<Self>,
    ) -> Result<
        crate::db::DatabaseOwnerRetirementReservationV1,
        Box<dyn StoreRuntimeOwnerAttachmentRetirementReservationV1>,
    > {
        Err(self)
    }

    fn preflight_commit(&self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.identity.validate()?;
        if self.fail_preflight {
            return Err(StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed {
                message: "injected owner preflight failure".to_owned(),
            });
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.identity.commit()?;
        if self.fail_commit {
            return Err(StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed {
                message: "injected owner commit failure".to_owned(),
            });
        }
        Ok(())
    }

    fn terminalize_after_commit_failure(&mut self) {
        self.terminalized.store(true, Ordering::SeqCst);
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn profile_shard(profile: &str) -> StoreShardIdV1 {
    StoreShardIdV1::profile(
        id::<BrainId>("brain.runtime-retirement"),
        id::<UserProfileId>(profile),
    )
}

fn project_shard(project: &str) -> StoreShardIdV1 {
    StoreShardIdV1::project(
        id::<BrainId>("brain.runtime-retirement"),
        id::<UserProfileId>("profile.runtime-retirement"),
        id::<ProjectId>(project),
    )
}

fn registry() -> StoreRuntimeRegistry {
    StoreRuntimeRegistry::new(Arc::new(UnusedResolver), Arc::new(UnusedPublisher))
}

fn authority(root: &std::path::Path, name: &str) -> DatabaseAuthority {
    let path = root.join(format!("{name}.db"));
    std::fs::write(&path, []).unwrap();
    DatabaseAuthority::acquire_test(&path, "retirement fixture authority").unwrap()
}

fn install_ready(
    registry: &StoreRuntimeRegistry,
    shard_id: StoreShardIdV1,
    authority: DatabaseAuthority,
    attachment: Box<dyn PhysicalRuntimeAttachment>,
) -> (StoreRuntimeBindingV1, Arc<StoreRuntimeOwnerAttachment>) {
    let binding = StoreRuntimeBindingV1::new(
        shard_id,
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    );
    let key = StoreRuntimeKey::from_binding(&binding);
    let runtime = Arc::new(ShardRuntime::new(binding.clone(), key.is_profile()));
    runtime
        .transition(RuntimeMaintenanceStateV1::Opening)
        .and_then(|()| runtime.transition(RuntimeMaintenanceStateV1::Ready))
        .unwrap();
    let verified = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        canonical_store_locator_digest(authority.canonical_database_path()).unwrap(),
    );
    let locator = super::super::RuntimeLocatorRecord::new(
        key.clone(),
        ResolvedStoreLocator::new(verified, authority.canonical_database_path().to_path_buf()),
    );
    let source = Arc::new(super::super::StoreRuntimeLeaseSource {
        publication: StoreRuntimeRegistryPublicationV1 {
            publication_id: RuntimePublicationIdV1::new(format!(
                "retirement-publication-{}",
                binding.authority_epoch.get()
            ))
            .unwrap(),
            binding: binding.clone(),
            published_at: super::super::utc_now(),
        },
        runtime,
        attachment: Arc::from(attachment),
        locator,
        opened_file_identity: crate::db::sqlite_generation_identity(
            authority.canonical_database_path(),
        )
        .unwrap(),
        database_authority: Some(authority),
        database_attachments: hotpath::mutex!(
            std::sync::Mutex::new(std::collections::BTreeMap::new()),
            label = "runtime_core.shard_runtime.database_attachments"
        ),
        next_database_attachment_id: std::sync::atomic::AtomicU64::new(1),
        next_database_owner_id: std::sync::atomic::AtomicU64::new(1),
        next_database_attachment_reservation_id: std::sync::atomic::AtomicU64::new(1),
    });
    let owner = Arc::new(StoreRuntimeOwnerAttachment { source });
    let mut state = registry.lock_state();
    if key.is_profile() {
        state
            .profile_authorities
            .insert(key.shard_id().clone(), binding.clone());
    }
    state.entries.insert(
        key,
        RegistryEntry::Ready(ReadyRuntime {
            owner: Arc::clone(&owner),
        }),
    );
    (binding, owner)
}

fn target(
    binding: &StoreRuntimeBindingV1,
    owner: &StoreRuntimeOwnerAttachment,
) -> StoreRuntimeRetirementTarget {
    StoreRuntimeRetirementTarget::new(
        binding.clone(),
        owner
            .database_authority
            .clone()
            .expect("retirement fixture installs an authority"),
    )
}

fn failing_owner_target(
    binding: &StoreRuntimeBindingV1,
    owner: &StoreRuntimeOwnerAttachment,
    fail_preflight: bool,
    fail_commit: bool,
    terminalized: Arc<AtomicBool>,
) -> StoreRuntimeRetirementTarget {
    let attachment = owner
        .issue_client_lease()
        .unwrap()
        .into_database_attachment()
        .unwrap();
    let owner_id = attachment.allocate_owner_identity().unwrap();
    let identity = attachment.reserve_for_owner(owner_id).unwrap();
    StoreRuntimeRetirementTarget::with_database_owner_attachment(
        binding.clone(),
        owner.database_authority.clone().unwrap(),
        Box::new(FailingOwnerAttachmentReservation {
            identity,
            _attachment: attachment,
            fail_preflight,
            fail_commit,
            terminalized,
        }),
    )
}

fn stale_owner_target(
    binding: &StoreRuntimeBindingV1,
    owner: &StoreRuntimeOwnerAttachment,
) -> StoreRuntimeRetirementTarget {
    let attachment = owner
        .issue_client_lease()
        .unwrap()
        .into_database_attachment()
        .unwrap();
    let owner_id = attachment.allocate_owner_identity().unwrap();
    let mut identity = attachment.reserve_for_owner(owner_id).unwrap();
    identity.owner_id =
        super::super::DatabaseRuntimeOwnerIdentityV1(owner_id.0.checked_add(1).unwrap());
    StoreRuntimeRetirementTarget::with_database_owner_attachment(
        binding.clone(),
        owner.database_authority.clone().unwrap(),
        Box::new(FailingOwnerAttachmentReservation {
            identity,
            _attachment: attachment,
            fail_preflight: false,
            fail_commit: false,
            terminalized: Arc::new(AtomicBool::new(false)),
        }),
    )
}

#[tokio::test]
async fn paired_graph_owner_target_reserves_and_restores_with_the_database_owner() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let authority = authority(directory.path(), "paired-graph-owner");
    let (binding, owner) = install_ready(
        &registry,
        project_shard("project.paired-graph-owner"),
        authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let (graph_owner, graph_target) = registry
        .attach_graph_store_owner(StoreRuntimeKey::from_binding(&binding))
        .await
        .unwrap();
    let database_attachment = owner
        .issue_client_lease()
        .unwrap()
        .into_database_attachment()
        .unwrap();
    let database_owner_id = database_attachment.allocate_owner_identity().unwrap();
    let database_identity = database_attachment
        .reserve_for_owner(database_owner_id)
        .unwrap();
    let target = StoreRuntimeRetirementTarget::with_owner_attachments(
        binding.clone(),
        owner.database_authority.clone().unwrap(),
        Box::new(FailingOwnerAttachmentReservation {
            identity: database_identity,
            _attachment: database_attachment,
            fail_preflight: false,
            fail_commit: false,
            terminalized: Arc::new(AtomicBool::new(false)),
        }),
        graph_target,
    );

    let StoreRuntimeRetirementResult::Reserved(mut reservation) =
        registry.reserve_retirement_batch(vec![target])
    else {
        panic!("the exact paired owner attachments must reserve together");
    };
    {
        let state = registry.lock_state();
        let publication = state
            .graph_publications
            .get(&StoreRuntimeKey::from_binding(&binding))
            .expect("reserved graph owner publication");
        assert!(matches!(
            publication.owner_attachment,
            Some(super::super::GraphStoreOwnerAttachmentState::OwnerReserved { .. })
        ));
    }

    reservation.cancel().unwrap();
    {
        let state = registry.lock_state();
        let publication = state
            .graph_publications
            .get(&StoreRuntimeKey::from_binding(&binding))
            .expect("cancelled graph owner publication");
        assert!(matches!(
            publication.owner_attachment,
            Some(super::super::GraphStoreOwnerAttachmentState::MapOwned { .. })
        ));
    }
    drop(graph_owner);
}

#[tokio::test]
async fn two_exact_owner_targets_survive_blocked_and_cancelled_retries() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let (first_binding, first_owner) = install_ready(
        &registry,
        project_shard("project.retry-owner-first"),
        authority(directory.path(), "retry-owner-first"),
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let (second_binding, second_owner) = install_ready(
        &registry,
        project_shard("project.retry-owner-second"),
        authority(directory.path(), "retry-owner-second"),
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let (first_graph_owner, first_graph_target) = registry
        .attach_graph_store_owner(StoreRuntimeKey::from_binding(&first_binding))
        .await
        .unwrap();
    let (second_graph_owner, second_graph_target) = registry
        .attach_graph_store_owner(StoreRuntimeKey::from_binding(&second_binding))
        .await
        .unwrap();
    let expected_first_graph_owner = {
        let state = registry.lock_state();
        state
            .graph_publications
            .get(&StoreRuntimeKey::from_binding(&first_binding))
            .expect("first graph publication exists")
            .owner_attachment
    };
    let expected_second_graph_owner = {
        let state = registry.lock_state();
        state
            .graph_publications
            .get(&StoreRuntimeKey::from_binding(&second_binding))
            .expect("second graph publication exists")
            .owner_attachment
    };

    let first_database_attachment = first_owner
        .issue_client_lease()
        .unwrap()
        .into_database_attachment()
        .unwrap();
    let first_database_identity = first_database_attachment
        .reserve_for_owner(first_database_attachment.allocate_owner_identity().unwrap())
        .unwrap();
    let expected_first_database_identity = first_database_identity.clone();
    let second_database_attachment = second_owner
        .issue_client_lease()
        .unwrap()
        .into_database_attachment()
        .unwrap();
    let second_database_identity = second_database_attachment
        .reserve_for_owner(
            second_database_attachment
                .allocate_owner_identity()
                .unwrap(),
        )
        .unwrap();
    let expected_second_database_identity = second_database_identity.clone();
    let first_target = StoreRuntimeRetirementTarget::with_owner_attachments(
        first_binding.clone(),
        first_owner.database_authority.clone().unwrap(),
        Box::new(FailingOwnerAttachmentReservation {
            identity: first_database_identity,
            _attachment: first_database_attachment,
            fail_preflight: false,
            fail_commit: false,
            terminalized: Arc::new(AtomicBool::new(false)),
        }),
        first_graph_target,
    );
    let second_target = StoreRuntimeRetirementTarget::with_owner_attachments(
        second_binding.clone(),
        second_owner.database_authority.clone().unwrap(),
        Box::new(FailingOwnerAttachmentReservation {
            identity: second_database_identity,
            _attachment: second_database_attachment,
            fail_preflight: false,
            fail_commit: false,
            terminalized: Arc::new(AtomicBool::new(false)),
        }),
        second_graph_target,
    );
    let blocker = first_owner.issue_client_lease().unwrap();

    let StoreRuntimeRetirementResult::Blocked(refusal) =
        registry.reserve_retirement_batch(vec![first_target, second_target])
    else {
        panic!("the live client must block both exact owner targets before reservation");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        StoreRuntimeRetirementBlocker::ClientLeases { binding, count: 1 }
            if binding.as_ref() == &first_binding
    )));
    let (_, retry_targets) = refusal.into_parts();
    assert_eq!(retry_targets.len(), 2);
    assert_eq!(
        retry_targets[0]
            .owner_attachment_identity()
            .expect("first target retains its database owner attachment")
            .attachment_id,
        expected_first_database_identity.attachment_id
    );
    assert_eq!(
        retry_targets[1]
            .owner_attachment_identity()
            .expect("second target retains its database owner attachment")
            .attachment_id,
        expected_second_database_identity.attachment_id
    );
    drop(blocker);

    let StoreRuntimeRetirementResult::Reserved(mut reservation) =
        registry.reserve_retirement_batch(retry_targets)
    else {
        panic!("the exact targets must reserve once their blocker releases");
    };
    let retry_targets = reservation.cancel().unwrap();
    assert_eq!(retry_targets.len(), 2);
    assert_eq!(
        retry_targets[0]
            .owner_attachment_identity()
            .expect("cancelled first target retains its database owner attachment")
            .attachment_id,
        expected_first_database_identity.attachment_id
    );
    assert_eq!(
        retry_targets[1]
            .owner_attachment_identity()
            .expect("cancelled second target retains its database owner attachment")
            .attachment_id,
        expected_second_database_identity.attachment_id
    );
    {
        let state = registry.lock_state();
        assert_eq!(
            state
                .graph_publications
                .get(&StoreRuntimeKey::from_binding(&first_binding))
                .expect("first graph publication remains after cancellation")
                .owner_attachment,
            expected_first_graph_owner
        );
        assert_eq!(
            state
                .graph_publications
                .get(&StoreRuntimeKey::from_binding(&second_binding))
                .expect("second graph publication remains after cancellation")
                .owner_attachment,
            expected_second_graph_owner
        );
    }

    let StoreRuntimeRetirementResult::Reserved(reservation) =
        registry.reserve_retirement_batch(retry_targets)
    else {
        panic!("cancelled exact owner targets must reserve on the second retry");
    };
    drop(reservation);
    drop(first_graph_owner);
    drop(second_graph_owner);
}

fn active_lease(binding: &StoreRuntimeBindingV1, lease_id: &str) -> RuntimeLeaseV1 {
    RuntimeLeaseV1 {
        lease_id: RuntimeLeaseIdV1::new(lease_id).unwrap(),
        binding: binding.clone(),
        holder: StoreClientIdV1::new("retirement-fixture-client").unwrap(),
        acquired_at: UtcMicros(0),
        expires_at: UtcMicros(i64::MAX),
    }
}

#[test]
fn preflight_is_all_or_none_and_foreign_identity_never_transitions_the_live_entry() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let initial_authority = authority(directory.path(), "first");
    let (binding, owner) = install_ready(
        &registry,
        profile_shard("profile.first"),
        initial_authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let foreign = StoreRuntimeBindingV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        StoreAuthorityEpochV1::new(2).unwrap(),
    );
    let foreign_authority = authority(directory.path(), "foreign-authority");

    let result = registry.reserve_retirement_batch(vec![
        target(&binding, &owner),
        target(&foreign, &owner),
        StoreRuntimeRetirementTarget::new(binding.clone(), foreign_authority),
    ]);
    assert!(matches!(
        result,
        StoreRuntimeRetirementResult::Blocked(refusal)
            if refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::BindingMismatch { expected, actual }
                    if expected.as_ref() == &foreign && actual.as_ref() == &binding
            )) && refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::AuthorityMismatch { binding: actual }
                    if actual.as_ref() == &binding
            ))
    ));
    let StoreRuntimeLookup::Ready(lease) = registry.lookup(&binding) else {
        panic!("all-or-none preflight must leave the matching runtime ready");
    };
    drop(lease);
}

#[test]
fn owner_preflight_failure_rolls_back_every_target_before_committing_any() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let first_authority = authority(directory.path(), "preflight-first");
    let second_authority = authority(directory.path(), "preflight-second");
    let (first_binding, first_owner) = install_ready(
        &registry,
        profile_shard("profile.preflight-first"),
        first_authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let (second_binding, second_owner) = install_ready(
        &registry,
        profile_shard("profile.preflight-second"),
        second_authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let terminalized = Arc::new(AtomicBool::new(false));
    let StoreRuntimeRetirementResult::Reserved(mut reservation) = registry
        .reserve_retirement_batch(vec![
            failing_owner_target(
                &first_binding,
                &first_owner,
                true,
                false,
                Arc::clone(&terminalized),
            ),
            target(&second_binding, &second_owner),
        ])
    else {
        panic!("preflight fixture must reserve both targets before commit");
    };

    assert!(matches!(
        reservation.commit(),
        Err(StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed { .. })
    ));
    assert!(!terminalized.load(Ordering::SeqCst));
    drop(reservation);
    assert!(matches!(
        registry.lookup(&first_binding),
        StoreRuntimeLookup::Ready(_)
    ));
    assert!(matches!(
        registry.lookup(&second_binding),
        StoreRuntimeLookup::Ready(_)
    ));
}

#[test]
fn stale_foreign_owner_identity_cannot_reclassify_the_canonical_attachment() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let authority = authority(directory.path(), "foreign-owner");
    let (binding, owner) = install_ready(
        &registry,
        profile_shard("profile.foreign-owner"),
        authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );

    let result = registry.reserve_retirement_batch(vec![stale_owner_target(&binding, &owner)]);
    assert!(matches!(
        result,
        StoreRuntimeRetirementResult::Blocked(refusal)
            if refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::OwnerAttachmentReservation { .. }
            ))
    ));
    assert!(matches!(
        registry.lookup(&binding),
        StoreRuntimeLookup::Ready(_)
    ));
}

#[test]
fn owner_commit_failure_is_terminal_truth_for_the_entire_batch() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let first_authority = authority(directory.path(), "commit-first");
    let second_authority = authority(directory.path(), "commit-second");
    let (first_binding, first_owner) = install_ready(
        &registry,
        profile_shard("profile.commit-first"),
        first_authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let (second_binding, second_owner) = install_ready(
        &registry,
        profile_shard("profile.commit-second"),
        second_authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let terminalized = Arc::new(AtomicBool::new(false));
    let StoreRuntimeRetirementResult::Reserved(mut reservation) = registry
        .reserve_retirement_batch(vec![
            failing_owner_target(
                &first_binding,
                &first_owner,
                false,
                true,
                Arc::clone(&terminalized),
            ),
            target(&second_binding, &second_owner),
        ])
    else {
        panic!("commit fixture must reserve both targets");
    };

    let commit = reservation.commit().unwrap();
    assert!(terminalized.load(Ordering::SeqCst));
    assert!(commit.outcomes().iter().all(|outcome| matches!(
        outcome,
        StoreRuntimeRetirementOutcome::Faulted {
            error: StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed { .. },
            ..
        }
    )));
    assert!(matches!(
        registry.lookup(&first_binding),
        StoreRuntimeLookup::Faulted { .. }
    ));
    assert!(matches!(
        registry.lookup(&second_binding),
        StoreRuntimeLookup::Faulted { .. }
    ));
}

#[test]
fn client_clone_and_direct_runtime_lease_block_then_release_for_retry() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let authority = authority(directory.path(), "client-and-direct");
    let (binding, owner) = install_ready(
        &registry,
        profile_shard("profile.client-and-direct"),
        authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let first = owner.issue_client_lease().unwrap();
    let first_clone = first.clone();
    let second = owner.issue_client_lease().unwrap();
    let direct = active_lease(&binding, "retirement.direct-lease");
    assert!(matches!(
        registry.acquire_lease(direct.clone()),
        super::super::StoreRuntimeLeaseAcquireResult::Acquired(_)
    ));

    let blocked = registry.reserve_retirement_batch(vec![target(&binding, &owner)]);
    assert!(matches!(
        blocked,
        StoreRuntimeRetirementResult::Blocked(refusal)
            if refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::ClientLeases { count: 2, .. }
            )) && refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::RuntimeLeases { count: 1, .. }
            ))
    ));

    drop(first);
    drop(first_clone);
    drop(second);
    assert!(registry.release_lease(&binding, &direct.lease_id));
    let StoreRuntimeRetirementResult::Reserved(reservation) =
        registry.reserve_retirement_batch(vec![target(&binding, &owner)])
    else {
        panic!("released independent tokens must permit a retry");
    };
    drop(reservation);
}

#[test]
fn independently_attached_database_facade_blocks_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let authority = authority(directory.path(), "database-facade");
    let (binding, owner) = install_ready(
        &registry,
        profile_shard("profile.database-facade"),
        authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let facade = owner
        .issue_client_lease()
        .unwrap()
        .into_database_attachment()
        .unwrap();

    let blocked = registry.reserve_retirement_batch(vec![target(&binding, &owner)]);
    assert!(matches!(
        blocked,
        StoreRuntimeRetirementResult::Blocked(refusal)
            if refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::DatabaseAttachments { count: 1, .. }
            ))
    ));

    drop(facade);
    let StoreRuntimeRetirementResult::Reserved(reservation) =
        registry.reserve_retirement_batch(vec![target(&binding, &owner)])
    else {
        panic!("releasing the facade must permit an exact retry");
    };
    drop(reservation);
}

#[test]
fn operation_profile_and_graph_leases_block_and_dropping_reservation_restores_ready() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let authority = authority(directory.path(), "operation-profile-graph");
    let (binding, owner) = install_ready(
        &registry,
        profile_shard("profile.operation-profile-graph"),
        authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let pin = match registry.profile_authority_pin(&binding.shard_id) {
        super::super::ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile fixture did not issue a pin: {other:?}"),
    };
    let client = owner.issue_client_lease().unwrap();
    let operation = client.begin_operation().unwrap();
    drop(client);
    {
        let mut state = registry.lock_state();
        state.graph_publications.insert(
            StoreRuntimeKey::from_binding(&binding),
            super::super::RetainedGraphPublication {
                binding: binding.clone(),
                verified_locator: owner.verified_locator().clone(),
                canonical_path: owner.canonical_path().to_path_buf(),
                owner_attachment: None,
                lease_tokens: [1].into_iter().collect(),
            },
        );
    }
    let blocked = registry.reserve_retirement_batch(vec![target(&binding, &owner)]);
    assert!(matches!(
        blocked,
        StoreRuntimeRetirementResult::Blocked(refusal)
            if refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::OperationLeases { count: 1, .. }
            )) && refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::ProfilePins { count: 1, .. }
            )) && refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::RetainedGraphLeases { count: 1, .. }
            )) && !refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                StoreRuntimeRetirementBlocker::ClientLeases { .. }
            ))
    ));

    drop(operation);
    drop(pin);
    registry.lock_state().graph_publications.clear();
    let StoreRuntimeRetirementResult::Reserved(reservation) =
        registry.reserve_retirement_batch(vec![target(&binding, &owner)])
    else {
        panic!("dropping every blocker must reserve the exact entry");
    };
    assert!(matches!(
        registry.acquire_lease(active_lease(&binding, "retirement.retiring")),
        super::super::StoreRuntimeLeaseAcquireResult::Rejected(
            StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. }
        )
    ));
    assert!(matches!(
        registry.profile_authority_pin(&binding.shard_id),
        super::super::ProfileAuthorityPinResult::Rejected(
            StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. }
        )
    ));
    let request = StoreRuntimeOpenRequest::new(binding.shard_id.clone(), binding.incarnation, None);
    assert!(matches!(
        registry.begin_or_join_open(&request),
        StoreRuntimeOpenBegin::Rejected(
            StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. }
        )
    ));
    drop(reservation);
    let StoreRuntimeLookup::Ready(lease) = registry.lookup(&binding) else {
        panic!("cancelling a reservation must restore its exact ready entry");
    };
    drop(lease);
}

#[test]
fn postcommit_drain_failure_is_faulted_and_never_restored_to_ready() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let authority = authority(directory.path(), "fault");
    let (binding, owner) = install_ready(
        &registry,
        profile_shard("profile.fault"),
        authority,
        Box::new(DrainFailure),
    );
    let StoreRuntimeRetirementResult::Reserved(mut reservation) =
        registry.reserve_retirement_batch(vec![target(&binding, &owner)])
    else {
        panic!("clean target must reserve before a physical failure");
    };
    let commit = reservation.commit().unwrap();
    assert!(matches!(
        commit.outcomes(),
        [StoreRuntimeRetirementOutcome::Faulted { .. }]
    ));
    assert!(matches!(
        registry.lookup(&binding),
        StoreRuntimeLookup::Faulted { .. }
    ));
}

#[test]
fn postcommit_irreversible_close_failure_is_durability_uncertain() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let authority = authority(directory.path(), "close-fault");
    let (binding, owner) = install_ready(
        &registry,
        profile_shard("profile.close-fault"),
        authority,
        Box::new(CloseFailure),
    );
    let StoreRuntimeRetirementResult::Reserved(mut reservation) =
        registry.reserve_retirement_batch(vec![target(&binding, &owner)])
    else {
        panic!("clean target must reserve before an irreversible close failure");
    };
    let commit = reservation.commit().unwrap();
    assert!(matches!(
        commit.outcomes(),
        [StoreRuntimeRetirementOutcome::DurabilityUncertain { .. }]
    ));
    assert!(matches!(
        registry.lookup(&binding),
        StoreRuntimeLookup::DurabilityUncertain { .. }
    ));
}

#[test]
fn reservations_are_one_shot_after_commit_or_cancellation() {
    let directory = tempfile::tempdir().unwrap();

    let committed_registry = registry();
    let committed_authority = authority(directory.path(), "committed-once");
    let (committed_binding, committed_owner) = install_ready(
        &committed_registry,
        profile_shard("profile.committed-once"),
        committed_authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let StoreRuntimeRetirementResult::Reserved(mut committed) = committed_registry
        .reserve_retirement_batch(vec![target(&committed_binding, &committed_owner)])
    else {
        panic!("clean target must reserve for one-shot commit");
    };
    assert!(matches!(
        committed.commit().unwrap().outcomes(),
        [StoreRuntimeRetirementOutcome::Closed { .. }]
    ));
    assert!(matches!(
        committed.commit(),
        Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed)
    ));

    let cancelled_registry = registry();
    let cancelled_authority = authority(directory.path(), "cancelled-once");
    let (cancelled_binding, cancelled_owner) = install_ready(
        &cancelled_registry,
        profile_shard("profile.cancelled-once"),
        cancelled_authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let StoreRuntimeRetirementResult::Reserved(mut cancelled) = cancelled_registry
        .reserve_retirement_batch(vec![target(&cancelled_binding, &cancelled_owner)])
    else {
        panic!("clean target must reserve for cancellation");
    };
    cancelled.cancel().unwrap();
    assert!(matches!(
        cancelled_registry.lookup(&cancelled_binding),
        StoreRuntimeLookup::Ready(_)
    ));
    assert!(matches!(
        cancelled.commit(),
        Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed)
    ));
    assert!(matches!(
        cancelled.cancel(),
        Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed)
    ));
}

#[tokio::test]
async fn retain_rejects_retiring_project_without_waiting_for_resolution() {
    let directory = tempfile::tempdir().unwrap();
    let registry = registry();
    let authority = authority(directory.path(), "retiring-project");
    let (binding, owner) = install_ready(
        &registry,
        project_shard("project.retiring"),
        authority,
        Box::new(EmptyPhysicalRuntimeAttachment),
    );
    let key = StoreRuntimeKey::from_binding(&binding);
    let StoreRuntimeRetirementResult::Reserved(reservation) =
        registry.reserve_retirement_batch(vec![target(&binding, &owner)])
    else {
        panic!("clean project target must reserve");
    };
    assert!(matches!(
        registry.retain_graph_store(key).await,
        Err(StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. })
    ));
    drop(reservation);
}
