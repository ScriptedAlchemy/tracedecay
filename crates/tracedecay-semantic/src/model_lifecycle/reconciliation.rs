impl SemanticModelLifecycleOwnerV1 {
    /// Reconcile store leases from the durable lifecycle authority.
    ///
    /// Lifecycle JSON and artifact inventory are separate crash-safe files.
    /// Restart and compensation therefore derive both lease slots from the
    /// committed lifecycle state before any artifact may be admitted.
    fn reconcile_embedding_artifact_leases(
        &self,
        durable: &DurableLifecycleV1,
        now_unix: u64,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let store_root = self.root.join("verified-artifacts").join("artifacts");
        let digest_for = |state: Option<&SemanticModelLifecycleStateV1>| {
            state
                .filter(|state| {
                    install_path_of(state).is_some_and(|path| path.starts_with(&store_root))
                })
                .map(|state| {
                    super::manifest::Sha256DigestHex::new(state.artifact_digest().to_owned())
                        .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)
                })
                .transpose()
        };
        let desired_active = digest_for(durable.state.as_ref())?;
        let desired_rollback = digest_for(durable.previous_ready.as_ref())?
            .filter(|digest| Some(digest) != desired_active.as_ref());
        let current_active = self.artifact_store.artifact_digest_for_lease(
            EMBEDDING_ACTIVE_LEASE_ID_V1,
            ArtifactLeaseKindV1::Active,
            now_unix,
        )?;
        match desired_active.as_ref() {
            Some(digest) => self.artifact_store.activate_artifact_with_rollback(
                digest,
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                EMBEDDING_ROLLBACK_LEASE_ID_V1,
                now_unix,
            )?,
            None => {
                if let Some(digest) = current_active {
                    self.artifact_store.release_artifact_lease(
                        &digest,
                        EMBEDDING_ACTIVE_LEASE_ID_V1,
                        ArtifactLeaseKindV1::Active,
                    )?;
                }
            }
        }
        let current_rollback = self.artifact_store.artifact_digest_for_lease(
            EMBEDDING_ROLLBACK_LEASE_ID_V1,
            ArtifactLeaseKindV1::Rollback,
            now_unix,
        )?;
        if current_rollback != desired_rollback {
            if let Some(digest) = current_rollback {
                self.artifact_store.release_artifact_lease(
                    &digest,
                    EMBEDDING_ROLLBACK_LEASE_ID_V1,
                    ArtifactLeaseKindV1::Rollback,
                )?;
            }
            if let Some(digest) = desired_rollback {
                self.artifact_store.acquire_artifact_lease(
                    &digest,
                    ArtifactLeaseV1 {
                        lease_id: EMBEDDING_ROLLBACK_LEASE_ID_V1.to_owned(),
                        kind: ArtifactLeaseKindV1::Rollback,
                        expires_at_unix: u64::MAX,
                    },
                    now_unix,
                )?;
            }
        }
        Ok(())
    }
}
