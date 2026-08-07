//! Artifact lease acquisition/release and runtime admission for
//! ModelArtifactStore. Split out of artifact_store.rs to keep the facade
//! under the 1000-line hygiene ceiling; pure structural move, no behavior
//! change.

use super::*;

impl ModelArtifactStore {
    /// Acquire or renew an active/rollback reference. Rollback leases also
    /// transition the record to the durable rollback-retained state.
    pub fn acquire_artifact_lease(
        &self,
        digest: &Sha256DigestHex,
        lease: ArtifactLeaseV1,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        if lease.lease_id.trim().is_empty() || lease.expires_at_unix <= now_unix {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        let record = inventory
            .records
            .get_mut(&digest.to_string())
            .ok_or(ArtifactImportErrorV1::StagingUnavailable)?;
        if !matches!(
            record.state,
            ArtifactInventoryStateV1::Installed | ArtifactInventoryStateV1::RetainedForRollback
        ) {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        if lease.kind == ArtifactLeaseKindV1::Rollback {
            record.state = ArtifactInventoryStateV1::RetainedForRollback;
        }
        let leases = inventory.leases.entry(digest.to_string()).or_default();
        leases
            .retain(|existing| existing.lease_id != lease.lease_id || existing.kind != lease.kind);
        leases.push(lease);
        self.save_inventory_locked(&inventory)
    }

    pub fn release_artifact_lease(
        &self,
        digest: &Sha256DigestHex,
        lease_id: &str,
        kind: ArtifactLeaseKindV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        if let Some(leases) = inventory.leases.get_mut(&digest.to_string()) {
            leases.retain(|lease| lease.lease_id != lease_id || lease.kind != kind);
            if leases.is_empty() {
                inventory.leases.remove(&digest.to_string());
            }
        }
        self.save_inventory_locked(&inventory)
    }

    pub fn artifact_digest_for_lease(
        &self,
        lease_id: &str,
        kind: ArtifactLeaseKindV1,
        now_unix: u64,
    ) -> Result<Option<Sha256DigestHex>, ArtifactImportErrorV1> {
        if lease_id.trim().is_empty() {
            return Err(ArtifactImportErrorV1::LeaseConflict);
        }
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let inventory = self.load_inventory_locked()?;
        let mut matched = inventory.leases.iter().filter_map(|(digest, leases)| {
            leases
                .iter()
                .any(|lease| {
                    lease.lease_id == lease_id
                        && lease.kind == kind
                        && lease.expires_at_unix > now_unix
                })
                .then_some(digest)
        });
        let first = matched.next();
        if matched.next().is_some() {
            return Err(ArtifactImportErrorV1::LeaseConflict);
        }
        first
            .map(|digest| {
                Sha256DigestHex::new(digest.clone())
                    .map_err(|_| ArtifactImportErrorV1::LeaseConflict)
            })
            .transpose()
    }

    /// Atomically activate one installed artifact and retain the prior active
    /// artifact as the single rollback target for this lease namespace.
    pub fn activate_artifact_with_rollback(
        &self,
        digest: &Sha256DigestHex,
        active_lease_id: &str,
        rollback_lease_id: &str,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        if active_lease_id.trim().is_empty()
            || rollback_lease_id.trim().is_empty()
            || active_lease_id == rollback_lease_id
        {
            return Err(ArtifactImportErrorV1::LeaseConflict);
        }
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        let record = inventory
            .records
            .get(&digest.to_string())
            .ok_or(ArtifactImportErrorV1::StagingUnavailable)?;
        if !matches!(
            record.state,
            ArtifactInventoryStateV1::Installed | ArtifactInventoryStateV1::RetainedForRollback
        ) {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }

        let mut prior_active = None;
        let mut prior_rollback = None;
        for (leased_digest, leases) in &inventory.leases {
            for lease in leases
                .iter()
                .filter(|lease| lease.expires_at_unix > now_unix)
            {
                let slot = if lease.kind == ArtifactLeaseKindV1::Active
                    && lease.lease_id == active_lease_id
                {
                    Some(&mut prior_active)
                } else if lease.kind == ArtifactLeaseKindV1::Rollback
                    && lease.lease_id == rollback_lease_id
                {
                    Some(&mut prior_rollback)
                } else {
                    None
                };
                if let Some(slot) = slot {
                    if slot
                        .as_ref()
                        .is_some_and(|existing: &String| existing != leased_digest)
                    {
                        return Err(ArtifactImportErrorV1::LeaseConflict);
                    }
                    *slot = Some(leased_digest.clone());
                }
            }
        }

        let digest_text = digest.to_string();
        let rollback_digest = if prior_active
            .as_ref()
            .is_some_and(|prior| prior != &digest_text)
        {
            prior_active
        } else {
            prior_rollback.filter(|prior| prior != &digest_text)
        }
        .filter(|rollback_digest| {
            inventory
                .records
                .get(rollback_digest)
                .is_some_and(|record| {
                    matches!(
                        record.state,
                        ArtifactInventoryStateV1::Installed
                            | ArtifactInventoryStateV1::RetainedForRollback
                    )
                })
        });
        for leases in inventory.leases.values_mut() {
            leases.retain(|lease| {
                !((lease.kind == ArtifactLeaseKindV1::Active && lease.lease_id == active_lease_id)
                    || (lease.kind == ArtifactLeaseKindV1::Rollback
                        && lease.lease_id == rollback_lease_id))
            });
        }
        inventory.leases.retain(|_, leases| !leases.is_empty());
        inventory
            .leases
            .entry(digest_text)
            .or_default()
            .push(ArtifactLeaseV1 {
                lease_id: active_lease_id.to_owned(),
                kind: ArtifactLeaseKindV1::Active,
                expires_at_unix: u64::MAX,
            });
        if let Some(rollback_digest) = rollback_digest {
            let rollback_record = inventory
                .records
                .get_mut(&rollback_digest)
                .ok_or(ArtifactImportErrorV1::LeaseConflict)?;
            rollback_record.state = ArtifactInventoryStateV1::RetainedForRollback;
            inventory
                .leases
                .entry(rollback_digest)
                .or_default()
                .push(ArtifactLeaseV1 {
                    lease_id: rollback_lease_id.to_owned(),
                    kind: ArtifactLeaseKindV1::Rollback,
                    expires_at_unix: u64::MAX,
                });
        }
        let retained_rollback_digests = inventory
            .leases
            .iter()
            .filter_map(|(leased_digest, leases)| {
                leases
                    .iter()
                    .any(|lease| {
                        lease.kind == ArtifactLeaseKindV1::Rollback
                            && lease.expires_at_unix > now_unix
                    })
                    .then_some(leased_digest.clone())
            })
            .collect::<BTreeSet<_>>();
        for (record_digest, record) in &mut inventory.records {
            if record.state == ArtifactInventoryStateV1::RetainedForRollback
                && !retained_rollback_digests.contains(record_digest)
            {
                record.state = ArtifactInventoryStateV1::Installed;
            }
        }
        self.save_inventory_locked(&inventory)
    }

    pub fn acquire_daemon_gc_lease(
        &self,
        lease_id: impl Into<String>,
        expires_at_unix: u64,
        now_unix: u64,
    ) -> Result<DaemonArtifactGcLeaseV1, ArtifactImportErrorV1> {
        let lease_id = lease_id.into();
        if lease_id.trim().is_empty() || expires_at_unix <= now_unix {
            return Err(ArtifactImportErrorV1::StoreBusy);
        }
        Ok(DaemonArtifactGcLeaseV1 {
            lease_id,
            expires_at_unix,
        })
    }

    /// Admit an installed artifact for runtime use against host evidence.
    /// Re-verifies the manifest and every on-disk member digest; any corrupt,
    /// revoked, quarantined, or incompatible artifact disables semantics.
    pub fn admit_for_runtime(
        &self,
        digest: &Sha256DigestHex,
        manifest: &ModelArtifactManifestV1,
        env: &RuntimeEnvironmentV1,
        now_unix: u64,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        self.admit_for_runtime_with_required_lease(digest, manifest, env, None, now_unix)
    }

    pub(super) fn admit_for_runtime_with_required_lease(
        &self,
        digest: &Sha256DigestHex,
        manifest: &ModelArtifactManifestV1,
        env: &RuntimeEnvironmentV1,
        required_lease: Option<(&str, ArtifactLeaseKindV1)>,
        now_unix: u64,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        self.verify_manifest(manifest)
            .map_err(|_| SemanticCapabilityDisabledV1::IdentityMismatch)?;
        let _lock = self
            .acquire_lock()
            .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
        self.recover_locked()
            .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
        let inventory = self
            .load_inventory_locked()
            .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
        let record = inventory
            .records
            .get(&digest.to_string())
            .ok_or(SemanticCapabilityDisabledV1::MissingArtifact)?;
        if let Some((lease_id, kind)) = required_lease
            && !inventory
                .leases
                .get(&digest.to_string())
                .is_some_and(|leases| {
                    leases.iter().any(|lease| {
                        lease.lease_id == lease_id
                            && lease.kind == kind
                            && lease.expires_at_unix > now_unix
                    })
                })
        {
            return Err(SemanticCapabilityDisabledV1::LeaseUnavailable);
        }
        match record.state {
            ArtifactInventoryStateV1::Installed | ArtifactInventoryStateV1::RetainedForRollback => {
            }
            ArtifactInventoryStateV1::Revoked => {
                return Err(SemanticCapabilityDisabledV1::RevokedArtifact);
            }
            ArtifactInventoryStateV1::Quarantined => {
                return Err(SemanticCapabilityDisabledV1::QuarantinedArtifact);
            }
            ArtifactInventoryStateV1::Staged | ArtifactInventoryStateV1::Verified => {
                return Err(SemanticCapabilityDisabledV1::MissingArtifact);
            }
        }
        if record.artifact_digest != *digest
            || *digest != manifest.artifact_identity_digest()
            || record.manifest_digest != manifest.canonical_digest()
            || record.members != manifest.payload.members
        {
            return Err(SemanticCapabilityDisabledV1::IdentityMismatch);
        }
        self.verify_artifact_record(record)
            .map_err(|_| SemanticCapabilityDisabledV1::CorruptArtifact)?;
        check_compatibility(&manifest.payload.runtime, env)?;
        check_resource_ceiling(&manifest.payload.resource_ceiling, env)?;
        let directory = self
            .artifacts_dir
            .open_dir_nofollow(digest.as_str())
            .map_err(|_| SemanticCapabilityDisabledV1::CorruptArtifact)?;
        Ok(AdmittedArtifactV1 {
            artifact_digest: digest.clone(),
            manifest_digest: manifest.canonical_digest(),
            manifest: manifest.clone(),
            source: Some(Arc::new(AdmittedArtifactSourceV1 { directory })),
        })
    }

    /// Re-admit an installed artifact from its durable canonical manifest and
    /// caller-supplied process evidence. Legacy records without that manifest
    /// remain unavailable rather than reconstructing authority from filenames
    /// or member rows.
    pub fn admit_for_runtime_by_digest(
        &self,
        digest: &Sha256DigestHex,
        env: &RuntimeEnvironmentV1,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        let manifest = {
            let inventory = self
                .inventory()
                .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
            inventory
                .records
                .get(&digest.to_string())
                .and_then(|record| record.manifest.clone())
                .ok_or(SemanticCapabilityDisabledV1::MissingArtifact)?
        };
        self.admit_for_runtime(digest, &manifest, env, 0)
    }

    pub fn admit_leased_for_runtime_by_digest(
        &self,
        digest: &Sha256DigestHex,
        env: &RuntimeEnvironmentV1,
        lease_id: &str,
        kind: ArtifactLeaseKindV1,
        now_unix: u64,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        let manifest = {
            let inventory = self
                .inventory()
                .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
            inventory
                .records
                .get(&digest.to_string())
                .and_then(|record| record.manifest.clone())
                .ok_or(SemanticCapabilityDisabledV1::MissingArtifact)?
        };
        self.admit_for_runtime_with_required_lease(
            digest,
            &manifest,
            env,
            Some((lease_id, kind)),
            now_unix,
        )
    }
}
