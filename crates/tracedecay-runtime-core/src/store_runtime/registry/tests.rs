mod attachment;
mod production_routes;
mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tracedecay_store::{RuntimeMaintenanceStateV1, StoreAuthorityEpochV1};

use super::*;
use support::*;

#[test]
fn budget_defaults_to_four_caps_at_eight_and_rejects_zero() {
    assert_eq!(
        StoreRuntimeRegistryConfig::default().project_code_open_runtime_budget(),
        DEFAULT_PROJECT_CODE_OPEN_RUNTIMES
    );
    assert!(StoreRuntimeRegistryConfig::new(MAX_PROJECT_CODE_OPEN_RUNTIMES).is_ok());
    for invalid in [0, MAX_PROJECT_CODE_OPEN_RUNTIMES + 1] {
        assert!(matches!(
            StoreRuntimeRegistryConfig::new(invalid),
            Err(StoreRuntimeRegistryFailure::InvalidProjectCodeBudget { requested, .. })
                if requested == invalid
        ));
    }
}

#[test]
fn exclusive_maintenance_budget_accepts_any_nonzero_exact_count() {
    for budget in [1, MAX_PROJECT_CODE_OPEN_RUNTIMES + 1, usize::MAX] {
        let config = StoreRuntimeRegistryConfig::for_exclusive_maintenance(budget).unwrap();
        assert_eq!(config.project_code_open_runtime_budget(), budget);
        let _ = registry(config);
    }
    assert!(matches!(
        StoreRuntimeRegistryConfig::for_exclusive_maintenance(0),
        Err(StoreRuntimeRegistryFailure::InvalidProjectCodeBudget {
            requested: 0,
            maximum: usize::MAX,
        })
    ));
}

#[tokio::test]
async fn concurrent_openers_publish_one_concrete_runtime_and_one_locator() {
    for round in 0..8 {
        let (registry, resolver, publisher) = registry(StoreRuntimeRegistryConfig::default());
        let pin = profile_pin(&registry).await;
        publisher.block.store(true, Ordering::SeqCst);
        let request = project_request(&format!("project.singleflight-{round}"), &pin);

        let mut joins = Vec::new();
        for index in 0..64 {
            match registry.begin_or_join_open(&request) {
                StoreRuntimeOpenBegin::Started(join) if index == 0 => joins.push(join),
                StoreRuntimeOpenBegin::Joined(join) => joins.push(join),
                other => panic!("unexpected open result: {other:?}"),
            }
        }
        wait_for_calls(&publisher.calls, 2).await;
        publisher.release.notify_one();
        let mut handles = Vec::new();
        for join in joins {
            match join.wait().await {
                StoreRuntimeOpenResult::Published(handle) => handles.push(handle),
                other @ StoreRuntimeOpenResult::Failed(_) => {
                    panic!("publication failed: {other:?}")
                }
            }
        }
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 2);
        assert!(
            handles[1..]
                .iter()
                .all(|handle| Arc::ptr_eq(handles[0].runtime(), handle.runtime()))
        );
        assert_eq!(
            handles[0].runtime().maintenance_state(),
            RuntimeMaintenanceStateV1::Ready
        );
    }
}

#[tokio::test]
async fn failed_open_wakes_joiners_and_retry_uses_a_higher_fence() {
    let (registry, _, publisher) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&registry).await;
    publisher.block.store(true, Ordering::SeqCst);
    publisher.mode.store(1, Ordering::SeqCst);
    let request = project_request("project.failure", &pin);
    let first = registry.begin_or_join_open(&request);
    wait_for_calls(&publisher.calls, 2).await;
    let second = registry.begin_or_join_open(&request);
    publisher.release.notify_one();
    let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(first.wait(), second.wait())
    })
    .await
    .expect("joiners cannot be stranded");
    for result in [first, second] {
        assert!(matches!(
            result,
            StoreRuntimeOpenResult::Failed(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "publish",
                ..
            })
        ));
    }

    let failed_epoch = publisher.bindings.lock().unwrap()[1].authority_epoch;
    publisher.block.store(false, Ordering::SeqCst);
    publisher.mode.store(0, Ordering::SeqCst);
    let retry = open_published(&registry, request).await;
    assert!(retry.binding().authority_epoch > failed_epoch);
}

#[test]
fn cancelled_open_task_wakes_every_joiner_and_allows_retry() {
    for round in 0..8 {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (registry, _, publisher) = registry(StoreRuntimeRegistryConfig::default());
        let (first, second, request) = runtime.block_on(async {
            let pin = profile_pin(&registry).await;
            publisher.block.store(true, Ordering::SeqCst);
            let request = project_request(&format!("project.cancelled-{round}"), &pin);
            let first = registry.begin_or_join_open(&request);
            wait_for_calls(&publisher.calls, 2).await;
            let second = registry.begin_or_join_open(&request);
            (first, second, request)
        });

        drop(runtime);
        let waiter = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        waiter.block_on(async {
            let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(first.wait(), second.wait())
            })
            .await
            .expect("cancelled opener cannot strand joiners");
            for result in [first, second] {
                assert!(matches!(
                    result,
                    StoreRuntimeOpenResult::Failed(
                        StoreRuntimeRegistryFailure::OpenTaskAbandoned { .. }
                    )
                ));
            }

            publisher.block.store(false, Ordering::SeqCst);
            open_published(&registry, request).await;
        });
    }
}

#[tokio::test]
async fn profile_pin_budget_and_all_runtime_blockers_are_authoritative() {
    let config = StoreRuntimeRegistryConfig::new(2).unwrap();
    let (registry, _, publisher) = registry(config);
    let pin = profile_pin(&registry).await;
    let held = open_published(&registry, code_request("worktree.held", &pin)).await;
    let leased = open_published(&registry, code_request("worktree.leased", &pin)).await;
    let leased_binding = leased.binding().clone();
    let lease = active_lease(&leased_binding, "lease.registry.blocker");
    assert!(matches!(
        registry.acquire_lease(lease.clone()),
        StoreRuntimeLeaseAcquireResult::Acquired(_)
    ));
    drop(leased);

    assert!(matches!(
        registry.begin_or_join_open(&code_request("worktree.overflow", &pin)),
        StoreRuntimeOpenBegin::Rejected(StoreRuntimeRegistryFailure::ProjectCodeBudgetExhausted {
            limit: 2
        })
    ));
    assert!(matches!(
        registry.lookup(pin.binding()),
        StoreRuntimeLookup::Ready(_)
    ));
    assert_eq!(publisher.runtime(2).health_snapshot().client_leases, 1);

    let held_runtime = Arc::downgrade(held.runtime());
    drop(held);
    open_published(&registry, code_request("worktree.overflow", &pin)).await;
    assert!(
        held_runtime.upgrade().is_none(),
        "eviction must release the canonical runtime after closing it"
    );
    assert!(registry.release_lease(&leased_binding, &lease.lease_id));

    publisher
        .runtime(0)
        .transition(RuntimeMaintenanceStateV1::Faulted)
        .unwrap();
    assert!(matches!(
        registry.profile_authority_pin(&profile_shard()),
        ProfileAuthorityPinResult::Rejected(
            StoreRuntimeRegistryFailure::ProfileAuthorityUnavailable {
                state: RuntimeMaintenanceStateV1::Faulted,
                ..
            }
        )
    ));
    assert!(matches!(
        registry.begin_or_join_open(&code_request("worktree.after-profile-fault", &pin)),
        StoreRuntimeOpenBegin::Rejected(StoreRuntimeRegistryFailure::ProfileAuthorityUnavailable {
            state: RuntimeMaintenanceStateV1::Faulted,
            ..
        })
    ));
}

#[tokio::test]
async fn profile_sessions_require_the_profile_pin_without_consuming_project_capacity() {
    let config = StoreRuntimeRegistryConfig::new(1).unwrap();
    let (registry, _, _) = registry(config);
    let pin = profile_pin(&registry).await;
    assert!(matches!(
        registry.begin_or_join_open(&StoreRuntimeOpenRequest::new(
            profile_sessions_shard(),
            incarnation(),
            None,
        )),
        StoreRuntimeOpenBegin::Rejected(
            StoreRuntimeRegistryFailure::ProfileAuthorityRequired { .. }
        )
    ));

    let profile_sessions = open_published(&registry, profile_sessions_request(&pin)).await;
    let project = open_published(&registry, project_request("project.full-budget", &pin)).await;

    assert!(matches!(
        registry.lookup(profile_sessions.binding()),
        StoreRuntimeLookup::Ready(_)
    ));
    assert!(matches!(
        registry.lookup(project.binding()),
        StoreRuntimeLookup::Ready(_)
    ));
}

#[tokio::test]
async fn project_memory_and_sessions_do_not_consume_code_capacity() {
    let config = StoreRuntimeRegistryConfig::new(1).unwrap();
    let (registry, _, _) = registry(config);
    let pin = profile_pin(&registry).await;
    let code = open_published(&registry, code_request("worktree.full-budget", &pin)).await;
    let project = open_published(
        &registry,
        project_request("project.memory-outside-code-budget", &pin),
    )
    .await;
    let sessions = open_published(
        &registry,
        project_sessions_request("project.sessions-outside-code-budget", &pin),
    )
    .await;

    assert!(matches!(
        registry.begin_or_join_open(&code_request("worktree.overflow", &pin)),
        StoreRuntimeOpenBegin::Rejected(StoreRuntimeRegistryFailure::ProjectCodeBudgetExhausted {
            limit: 1
        })
    ));
    for binding in [code.binding(), project.binding(), sessions.binding()] {
        assert!(matches!(
            registry.lookup(binding),
            StoreRuntimeLookup::Ready(_)
        ));
    }
}

#[tokio::test]
async fn epochs_are_monotonic_across_registries_and_respect_a_retained_floor() {
    let resolver: Arc<dyn StoreRuntimeResolver> = Arc::new(TestResolver::default());
    let publisher: Arc<dyn ShardRuntimePublisher> = Arc::new(TestPublisher::default());
    let floor = StoreAuthorityEpochV1::new(1_000_000).unwrap();
    let first = StoreRuntimeRegistry::with_config_and_authority_epoch_floor(
        resolver.clone(),
        publisher.clone(),
        StoreRuntimeRegistryConfig::default(),
        Some(floor),
    )
    .unwrap();
    let first = open_published(
        &first,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let second = StoreRuntimeRegistry::new(resolver, publisher);
    let second = open_published(
        &second,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    assert!(first.binding().authority_epoch > floor);
    assert!(second.binding().authority_epoch > first.binding().authority_epoch);
    assert!(!Arc::ptr_eq(first.runtime(), second.runtime()));
}

#[test]
fn exact_sql_authority_rechecks_opened_file_identity() {
    use tracedecay_rusqlite_runtime::exact_sql::{
        ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
    };

    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("identity.db");
    std::fs::write(&database_path, []).unwrap();
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&database_path, "exact SQL identity test")
            .unwrap();
    let current_identity = crate::db::sqlite_generation_identity(&database_path).unwrap();
    let authority = RuntimeDatabaseWriteAuthority {
        authority,
        canonical_path: database_path.canonicalize().unwrap(),
        opened_file_identity: current_identity.wrapping_add(1),
    };

    assert!(matches!(
        ExactSqlWriteAuthority::verify(&authority, ExactSqlWriteIntent::Execute),
        Err(ExactSqlError::AuthorityDenied(message))
            if message == "database file identity changed after registry attachment"
    ));
}
