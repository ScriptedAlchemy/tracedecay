//! Retained-runtime registration identity: canonical authority, not objects.

use super::*;
use std::collections::BTreeSet;
use tracedecay_application::retained_surfaces::RetainedSurfacePortsV1;
use tracedecay_domain::{BrainId, RepositoryId, WorktreeId};
use tracedecay_store::{
    StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    VerifiedStoreLocatorV1, canonical_store_locator_digest,
};

fn scope(worktree: &str) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.retained.identity").expect("project"),
        RepositoryId::new("repository.retained.identity").expect("repository"),
        WorktreeId::new(worktree).expect("worktree"),
        None,
    )
    .expect("scope")
}

fn actor(name: &str) -> ActorId {
    ActorId::new(name).expect("actor")
}

fn grant(
    scope: &ResolvedScope,
    actor: &ActorId,
    policy: &str,
    expires_at: i64,
) -> CapabilityGrantSnapshot {
    CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.retained.identity.{expires_at}")).expect("grant id"),
        1,
        canonical_sha256(&("tracedecay.test.retained-grant-policy", policy)).expect("digest"),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(expires_at),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.retained.fixture").expect("capability")]),
        BTreeSet::from([UseCaseId::new("use-case.retained.fixture").expect("use case")]),
        DisclosureClass::Sensitive,
    )
    .expect("grant")
}

fn store(path: &str, epoch: u64) -> RetainedRuntimeStoreAuthorityV1 {
    let shard_id = StoreShardIdV1::project(
        BrainId::new("brain.retained.identity").expect("brain"),
        UserProfileId::new("profile.retained.identity").expect("profile"),
        ProjectId::new("project.retained.identity").expect("project"),
    );
    let incarnation = StoreIncarnationV1::new(1).expect("incarnation");
    RetainedRuntimeStoreAuthorityV1::new(
        StoreRuntimeBindingV1::new(
            shard_id.clone(),
            incarnation,
            StoreAuthorityEpochV1::new(epoch).expect("epoch"),
        ),
        VerifiedStoreLocatorV1::new(
            shard_id,
            incarnation,
            canonical_store_locator_digest(Path::new(path)).expect("locator digest"),
        ),
    )
}

fn ports() -> Arc<RetainedSurfacePortsV1<'static>> {
    Arc::new(RetainedSurfacePortsV1::default())
}

async fn registered(
    service: &DaemonInvocationService,
    project_root: &Path,
) -> RegisteredRetainedRuntime {
    service
        .project_runtimes
        .get::<RegisteredRetainedRuntime>(project_root)
        .await
        .expect("retained runtime must stay registered")
}

#[tokio::test]
async fn same_authority_reopen_rebinds_fresh_ports_and_renews_grant() {
    let service = DaemonInvocationService::default();
    let registrar = DaemonRetainedRuntimeRegistrar::new(&service);
    let project_root = PathBuf::from("/retained-identity/reopen");
    let scope = scope("worktree.retained.identity");
    let actor = actor("actor.retained.identity");
    let store = store("/retained-identity/store", 1);

    let first_ports = ports();
    registrar
        .register(
            project_root.clone(),
            scope.clone(),
            actor.clone(),
            grant(&scope, &actor, "policy.a", 10),
            store.clone(),
            Arc::clone(&first_ports),
        )
        .await
        .expect("first registration");
    assert!(
        registered(&service, &project_root)
            .await
            .ports_ptr_eq(&first_ports)
    );

    // A reopen constructs new ports over the same store and mints its grant
    // from the then-current configuration revision; it must rebind both, never
    // be refused as a foreign runtime.
    let reopened_ports = ports();
    assert!(!Arc::ptr_eq(&first_ports, &reopened_ports));
    let reopened_grant = grant(&scope, &actor, "policy.b", 20);
    registrar
        .register(
            project_root.clone(),
            scope.clone(),
            actor.clone(),
            reopened_grant.clone(),
            store,
            Arc::clone(&reopened_ports),
        )
        .await
        .expect("same-authority reopen must rebind");
    let current = registered(&service, &project_root).await;
    assert!(
        current.ports_ptr_eq(&reopened_ports),
        "live route must own the ports"
    );
    assert!(!current.ports_ptr_eq(&first_ports));
    assert_eq!(
        current.grant, reopened_grant,
        "the live route's grant supersedes the retired route's"
    );
}

#[tokio::test]
async fn foreign_scope_actor_or_store_is_refused_and_leaves_incumbent_intact() {
    let service = DaemonInvocationService::default();
    let registrar = DaemonRetainedRuntimeRegistrar::new(&service);
    let project_root = PathBuf::from("/retained-identity/foreign");
    let scope = scope("worktree.retained.identity");
    let actor = actor("actor.retained.identity");
    let store = store("/retained-identity/store", 1);
    let incumbent_ports = ports();
    registrar
        .register(
            project_root.clone(),
            scope.clone(),
            actor.clone(),
            grant(&scope, &actor, "policy.a", 10),
            store.clone(),
            Arc::clone(&incumbent_ports),
        )
        .await
        .expect("incumbent registration");

    let other_scope = self::scope("worktree.retained.other");
    let other_actor = self::actor("actor.retained.other");
    let foreign: [(
        &str,
        ResolvedScope,
        ActorId,
        CapabilityGrantSnapshot,
        RetainedRuntimeStoreAuthorityV1,
    ); 4] = [
        (
            "different store locator under the same apparent project identity",
            scope.clone(),
            actor.clone(),
            grant(&scope, &actor, "policy.a", 20),
            self::store("/retained-identity/other-store", 1),
        ),
        (
            "different publication epoch of the same store",
            scope.clone(),
            actor.clone(),
            grant(&scope, &actor, "policy.a", 20),
            self::store("/retained-identity/store", 2),
        ),
        (
            "different worktree scope",
            other_scope.clone(),
            actor.clone(),
            grant(&other_scope, &actor, "policy.a", 20),
            store.clone(),
        ),
        (
            "different actor",
            scope.clone(),
            other_actor.clone(),
            grant(&scope, &other_actor, "policy.a", 20),
            store.clone(),
        ),
    ];
    for (case, scope, actor, grant, store) in foreign {
        let error = registrar
            .register(project_root.clone(), scope, actor, grant, store, ports())
            .await
            .expect_err(case);
        assert!(
            matches!(&error, TraceDecayError::Config { message } if message.contains("store authority")),
            "{case}: {error}"
        );
        let current = registered(&service, &project_root).await;
        assert!(
            current.ports_ptr_eq(&incumbent_ports),
            "{case}: incumbent ports replaced"
        );
        assert_eq!(
            current.grant.expires_at,
            UtcMicros(10),
            "{case}: incumbent grant renewed"
        );
    }
}

#[tokio::test]
async fn concurrent_equal_opens_settle_on_exactly_one_route_and_refuse_the_foreign_one() {
    let service = DaemonInvocationService::default();
    let registrar = DaemonRetainedRuntimeRegistrar::new(&service);
    let project_root = PathBuf::from("/retained-identity/concurrent");
    let scope = scope("worktree.retained.identity");
    let actor = actor("actor.retained.identity");
    let store = store("/retained-identity/store", 1);

    let equal_ports = (0..8).map(|_| ports()).collect::<Vec<_>>();
    let foreign_store = self::store("/retained-identity/other-store", 1);
    let foreign_ports = ports();
    let equal = equal_ports.iter().map(|ports| {
        registrar.register(
            project_root.clone(),
            scope.clone(),
            actor.clone(),
            grant(&scope, &actor, "policy.a", 10),
            store.clone(),
            Arc::clone(ports),
        )
    });
    let foreign = registrar.register(
        project_root.clone(),
        scope.clone(),
        actor.clone(),
        grant(&scope, &actor, "policy.a", 10),
        foreign_store,
        Arc::clone(&foreign_ports),
    );
    let (equal_outcomes, foreign_outcome) =
        tokio::join!(futures_util::future::join_all(equal), foreign);
    for outcome in equal_outcomes {
        outcome.expect("every equal open must succeed");
    }
    foreign_outcome.expect_err("the foreign store must be refused even while equal opens race");

    let current = registered(&service, &project_root).await;
    let bound = equal_ports
        .iter()
        .filter(|ports| current.ports_ptr_eq(ports))
        .count();
    assert_eq!(
        bound, 1,
        "exactly one route's ports are bound; none are mixed"
    );
    assert!(!current.ports_ptr_eq(&foreign_ports));
    assert!(
        service
            .project_runtimes
            .holds::<RegisteredRetainedRuntime>(&project_root)
            .await,
        "racing opens must not retire the route"
    );
}
