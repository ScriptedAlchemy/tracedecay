use tracedecay_store::{StoreShardScopeV1, graph_store_locator_path};

use super::{
    LocalStoreLocatorResolutionV1, LocalStoreLocatorUnavailableReasonV1,
    LocalStoreLocatorUnavailableV1, LocalStoreRuntimeResolverV1, StoreRuntimeKey,
    canonical_or_prospective_regular_file, local_filesystem_safety, verified_locator,
};

impl LocalStoreRuntimeResolverV1 {
    /// Resolves the Grafeo database file paired with one exact project/profile
    /// relational authority.
    ///
    /// Code scopes are namespaces inside their owning project graph runtime,
    /// not physical stores. Callers must retain the project graph key through
    /// `StoreRuntimeRegistry::retain_code_graph_store`; resolving a code shard
    /// here fails closed instead of recreating per-worktree graph sharding.
    pub fn resolve_graph_key(&self, key: &StoreRuntimeKey) -> LocalStoreLocatorResolutionV1 {
        #[cfg(test)]
        tests::spin_injected_resolution_cpu(key);
        if matches!(key.shard_id().scope, StoreShardScopeV1::Code { .. }) {
            return LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                shard_id: key.shard_id().clone(),
                reason: LocalStoreLocatorUnavailableReasonV1::UnsupportedShardScope,
            });
        }
        let resolved = self
            .resolve_key_inner(key, &local_filesystem_safety)
            .and_then(|store| {
                let metadata = store.metadata().clone();
                let graph_path = graph_store_locator_path(
                    &metadata.canonical_store_root,
                    store.locator().path(),
                )
                .map_err(|_| LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)?;
                let graph_path = canonical_or_prospective_regular_file(
                    &graph_path,
                    &metadata.canonical_store_root,
                )?;
                verified_locator(
                    key,
                    metadata.kind,
                    metadata.canonical_profile_root,
                    metadata.canonical_store_root,
                    graph_path,
                    &local_filesystem_safety,
                )
            });
        match resolved {
            Ok(locator) => LocalStoreLocatorResolutionV1::Resolved(locator),
            Err(reason) => {
                LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                    shard_id: key.shard_id().clone(),
                    reason,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use tracedecay_graph_db::{
        GraphDbLeaseV1, GraphDbOwnerRegistrationV1, GraphDbRegistration, GraphDbRegistry,
        GraphDbRegistryConfig, NeverCancelled,
    };
    use tracedecay_runtime_core::storage;
    use tracedecay_store::{
        BrainId, ProjectId, RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
        RetainedGraphStoreOwnerOperationLeaseErrorV1, StoreAuthorityEpochV1, StoreIncarnationV1,
        StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
    };

    use super::super::{
        LocalProfileStoreAuthorityV1, LocalProjectEnrollmentAuthorityV1,
        LocalStoreLocatorResolutionV1, LocalStoreRuntimeResolverV1, StoreRuntimeKey,
    };

    #[derive(Debug)]
    struct TestGraphLease {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        canonical_path: std::path::PathBuf,
    }

    impl RetainedGraphStoreLeaseV1 for TestGraphLease {
        fn binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.verified_locator
        }

        fn canonical_path(&self) -> &std::path::Path {
            &self.canonical_path
        }
    }

    impl RetainedGraphStoreOwnerAttachmentV1 for TestGraphLease {
        fn binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.verified_locator
        }

        fn canonical_path(&self) -> &std::path::Path {
            &self.canonical_path
        }

        fn issue_operation_lease(
            &self,
        ) -> Result<Arc<dyn RetainedGraphStoreLeaseV1>, RetainedGraphStoreOwnerOperationLeaseErrorV1>
        {
            Ok(Arc::new(Self {
                binding: self.binding.clone(),
                verified_locator: self.verified_locator.clone(),
                canonical_path: self.canonical_path.clone(),
            }))
        }
    }

    fn mount_and_resolve(
        registry: &GraphDbRegistry,
        registration: GraphDbRegistration,
    ) -> Result<GraphDbLeaseV1, tracedecay_graph_db::GraphDbError> {
        let operation = registration.clone();
        let authority_attachment = Box::new(TestGraphLease {
            binding: registration.authority_lease.binding().clone(),
            verified_locator: registration.authority_lease.verified_locator().clone(),
            canonical_path: registration.authority_lease.canonical_path().to_path_buf(),
        });
        let owner_attachment = registry.resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
            operation: registration,
            authority_attachment,
        })?;
        let lease = registry.resolve(operation)?;
        drop(owner_attachment);
        Ok(lease)
    }

    /// The locator this resolver derives for a project graph is the one the
    /// graph registry accepts: it sits beside the relational store under the
    /// typed project's sharded root and opens without a rebuild.
    #[test]
    fn canonical_runtime_resolver_locator_opens_through_graph_registry() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        std::fs::create_dir(&profile_root).unwrap();
        std::fs::create_dir(&project_root).unwrap();

        let binding = StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                BrainId::try_from("brain-a".to_owned()).unwrap(),
                UserProfileId::try_from("profile-a".to_owned()).unwrap(),
                ProjectId::try_from("project-a".to_owned()).unwrap(),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        );
        // No repo-local enrollment marker exists any more: the typed enrollment
        // authority below is the identity authority and store paths derive from
        // the typed project id, never from the root.
        let store_root = storage::profile_sharded_data_root(&profile_root, "project-a");
        std::fs::create_dir_all(&store_root).unwrap();

        let resolver = LocalStoreRuntimeResolverV1::new(LocalProfileStoreAuthorityV1::new(
            binding.shard_id.brain_id.clone(),
            binding.shard_id.profile_id.clone(),
            profile_root,
        ));
        resolver
            .register_project_authority(LocalProjectEnrollmentAuthorityV1::new(
                ProjectId::try_from("project-a".to_owned()).unwrap(),
                [project_root],
            ))
            .unwrap();
        let key = StoreRuntimeKey::new(binding.shard_id.clone(), binding.incarnation);
        let resolved = match resolver.resolve_graph_key(&key) {
            LocalStoreLocatorResolutionV1::Resolved(locator) => locator,
            LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
                panic!("expected canonical graph locator: {unavailable:?}")
            }
        };
        assert_eq!(
            resolved.locator().path().parent(),
            Some(store_root.as_path())
        );
        assert_eq!(
            resolved.locator().path().extension(),
            Some(std::ffi::OsStr::new("grafeo"))
        );

        let registration = GraphDbRegistration {
            authority_lease: Arc::new(TestGraphLease {
                binding,
                verified_locator: resolved.locator().verified().clone(),
                canonical_path: resolved.locator().path().to_path_buf(),
            }),
            cancellation: Arc::new(NeverCancelled),
            lifecycle_cancellation: Arc::new(NeverCancelled),
            deadline: std::time::Instant::now() + Duration::from_secs(30),
        };
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();

        let database = mount_and_resolve(&registry, registration).unwrap();
        assert!(database.snapshot().is_ok());
    }

    static INJECTED_RESOLUTION_CPU: std::sync::Mutex<std::collections::BTreeMap<String, Duration>> =
        std::sync::Mutex::new(std::collections::BTreeMap::new());

    pub(super) fn spin_injected_resolution_cpu(key: &StoreRuntimeKey) {
        let cpu = INJECTED_RESOLUTION_CPU
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&format!("{:?}", key.shard_id()));
        if let Some(cpu) = cpu {
            let spun = std::time::Instant::now();
            while spun.elapsed() < cpu {
                std::hint::spin_loop();
            }
        }
    }

    /// Graph store attachment awaits this resolution from native tasks: the
    /// code-index worker and the retained graph-open hook tasks. A debug
    /// build fails a native task whose poll runs past `LONG_POLL_CPU`, so the
    /// resolution must not run inside the poll (#4066).
    #[cfg(all(debug_assertions, unix))]
    #[test]
    fn a_graph_resolution_longer_than_one_native_poll_does_not_fail_its_task() {
        use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome};
        use tracedecay_runtime_core::shard_runtime::registry::StoreRuntimeResolver;

        let temporary = TempDir::new().unwrap();
        let brain = BrainId::try_from("brain-long-resolve".to_owned()).unwrap();
        let profile = UserProfileId::try_from("profile-long-resolve".to_owned()).unwrap();
        let resolver = LocalStoreRuntimeResolverV1::new(LocalProfileStoreAuthorityV1::new(
            brain.clone(),
            profile.clone(),
            temporary.path().canonicalize().unwrap(),
        ));
        let key = StoreRuntimeKey::new(
            StoreShardIdV1::project(
                brain,
                profile,
                ProjectId::try_from("project-long-resolve".to_owned()).unwrap(),
            ),
            StoreIncarnationV1::new(1).unwrap(),
        );
        INJECTED_RESOLUTION_CPU
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                format!("{:?}", key.shard_id()),
                native_runtime::LONG_POLL_CPU * 2,
            );
        // The unenrolled project does not resolve; only where it ran is under test.
        let resolution = NativeTask::spawn(async move {
            let _ = resolver.resolve_graph(&key).await;
        })
        .expect("native resolution task");
        assert_eq!(
            native_runtime::block_on(resolution.wait()).expect("process runtime"),
            NativeTaskOutcome::Completed(()),
        );
    }
}
