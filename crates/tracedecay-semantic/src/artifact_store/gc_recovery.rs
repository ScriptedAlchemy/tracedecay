//! Garbage collection and crash-recovery flows for ModelArtifactStore.
//! Split out of artifact_store.rs to keep the facade under the 1000-line
//! hygiene ceiling; pure structural move, no behavior change.

use super::*;

impl ModelArtifactStore {
    /// Garbage-collect unreferenced artifacts past the grace window.
    /// `RetainedForRollback`, `Revoked`, and `Installed` records are never
    /// collected here; each removal appends one receipt to
    /// `receipts/gc.jsonl`.
    #[cfg(test)]
    pub fn gc(&self, now_unix: u64) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
        self.gc_locked_by_policy(now_unix, false)
    }

    /// Collect installed artifacts only during an explicit daemon lease and
    /// only when no unexpired active/rollback reference protects them.
    pub fn gc_with_daemon_lease(
        &self,
        lease: &DaemonArtifactGcLeaseV1,
        now_unix: u64,
    ) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
        if lease.lease_id.trim().is_empty() || lease.expires_at_unix <= now_unix {
            return Err(ArtifactImportErrorV1::StoreBusy);
        }
        self.gc_locked_by_policy(now_unix, true)
    }

    pub(super) fn gc_locked_by_policy(
        &self,
        now_unix: u64,
        include_unleased_installed: bool,
    ) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        let records: Vec<ArtifactInventoryRecordV1> = inventory
            .records
            .values()
            .filter(|r| {
                let collectible_state = matches!(
                    r.state,
                    ArtifactInventoryStateV1::Verified | ArtifactInventoryStateV1::Quarantined
                ) || (include_unleased_installed
                    && matches!(
                        r.state,
                        ArtifactInventoryStateV1::Installed
                            | ArtifactInventoryStateV1::RetainedForRollback
                    ));
                let has_live_reference = inventory
                    .leases
                    .get(&r.artifact_digest.to_string())
                    .is_some_and(|leases| {
                        leases.iter().any(|lease| lease.expires_at_unix > now_unix)
                    });
                collectible_state
                    && !has_live_reference
                    && now_unix.saturating_sub(r.recorded_at_unix) >= self.retention.grace_seconds
            })
            .cloned()
            .collect();
        if records.is_empty() {
            return Ok(Vec::new());
        }
        self.write_recovery_locked(&RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Gc {
                recorded_at_unix: now_unix,
                records: records.clone(),
            },
        })?;
        for record in &records {
            self.remove_artifact_record(record)?;
            inventory
                .records
                .remove(&record.artifact_digest.to_string());
            inventory.leases.remove(&record.artifact_digest.to_string());
        }
        self.save_inventory_locked(&inventory)?;
        let receipts: Vec<GcReceiptV1> = records
            .into_iter()
            .map(|record| GcReceiptV1 {
                artifact_digest: record.artifact_digest,
                removed_at_unix: now_unix,
                prior_state: record.state,
            })
            .collect();
        self.append_receipts_locked(&receipts)?;
        self.clear_recovery_locked()?;
        Ok(receipts)
    }

    pub(super) fn record_for(
        &self,
        manifest: &ModelArtifactManifestV1,
        state: ArtifactInventoryStateV1,
        recorded_at_unix: u64,
        quarantine_reason: Option<QuarantineReasonV1>,
    ) -> ArtifactInventoryRecordV1 {
        ArtifactInventoryRecordV1 {
            artifact_digest: manifest.artifact_identity_digest(),
            manifest_digest: manifest.canonical_digest(),
            manifest: Some(manifest.clone()),
            members: manifest.payload.members.clone(),
            state,
            recorded_at_unix,
            quarantine_reason,
        }
    }

    pub(super) fn ensure_session_dir(&self, session: &ImportSession) -> Result<(), ArtifactImportErrorV1> {
        if self.staging_dir_for(&session.staging_id)? != session.staging_path {
            return Err(ArtifactImportErrorV1::UnsafeStagingHandle);
        }
        session
            .staging_dir
            .dir_metadata()
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        session
            .members_dir
            .dir_metadata()
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        Ok(())
    }

    pub(super) fn ensure_session_active_locked(
        &self,
        session: &ImportSession,
    ) -> Result<(), ArtifactImportErrorV1> {
        let inventory = self.load_inventory_locked()?;
        let state = inventory
            .records
            .get(&session.meta.manifest_identity_digest.to_string())
            .map(|record| record.state);
        if matches!(
            state,
            Some(ArtifactInventoryStateV1::Quarantined | ArtifactInventoryStateV1::Revoked)
        ) {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        Ok(())
    }

    pub(super) fn staging_meta_matches(
        &self,
        meta: &StagingMetaV1,
        manifest: &ModelArtifactManifestV1,
    ) -> bool {
        meta.schema == STAGING_SCHEMA_V1
            && meta.manifest == *manifest
            && meta.manifest_identity_digest == manifest.artifact_identity_digest()
            && meta
                .members
                .iter()
                .map(|member| &member.member)
                .eq(manifest.payload.members.iter())
    }

    pub(super) fn staging_member_lengths_match(
        &self,
        session: &ImportSession,
    ) -> Result<bool, ArtifactImportErrorV1> {
        self.ensure_session_dir(session)?;
        for staged in &session.meta.members {
            let file = match open_cap_file(
                &session.members_dir,
                member_file_name(staged.member.role),
                true,
                false,
                false,
                false,
                false,
            ) {
                Ok(file) => file,
                Err(ArtifactImportErrorV1::StagingUnavailable) => return Ok(false),
                Err(error) => return Err(error),
            };
            let metadata = file
                .metadata()
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            if metadata.len() != staged.bytes_written
                || staged.bytes_written > staged.member.byte_length
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn write_recovery_locked(
        &self,
        journal: &RecoveryJournalV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        let bytes =
            serde_json::to_vec(journal).map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        atomic_write_cap_file(
            &self.root_dir,
            &self.root,
            ".artifact-store-recovery.json",
            &bytes,
        )
    }

    pub(super) fn clear_recovery_locked(&self) -> Result<(), ArtifactImportErrorV1> {
        remove_cap_file_if_exists(&self.root_dir, ".artifact-store-recovery.json")?;
        sync_cap_dir(&self.root_dir)?;
        Ok(())
    }

    pub(super) fn recover_locked(&self) -> Result<(), ArtifactImportErrorV1> {
        if let Some(bytes) =
            read_optional_cap_file(&self.root_dir, ".artifact-store-recovery.json")?
        {
            let journal: RecoveryJournalV1 = serde_json::from_slice(&bytes)
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            if journal.schema != RECOVERY_SCHEMA_V1 {
                return Err(ArtifactImportErrorV1::StorageFailure);
            }
            match journal.action {
                RecoveryActionV1::Install { record, staging_id } => {
                    self.recover_install_locked(*record, &staging_id)?;
                }
                RecoveryActionV1::Gc {
                    recorded_at_unix,
                    records,
                } => {
                    self.recover_gc_locked(records, recorded_at_unix)?;
                }
            }
            self.clear_recovery_locked()?;
        }
        self.recover_staged_imports_locked()
    }

    pub(super) fn recover_install_locked(
        &self,
        record: ArtifactInventoryRecordV1,
        staging_id: &str,
    ) -> Result<(), ArtifactImportErrorV1> {
        if !is_valid_staging_id(staging_id) {
            return Err(ArtifactImportErrorV1::UnsafeStagingHandle);
        }
        let staging_exists = self.staging_dir.open_dir_nofollow(staging_id).is_ok();
        match self
            .artifacts_dir
            .symlink_metadata(record.artifact_digest.as_str())
        {
            Ok(_) => {
                self.verify_artifact_record(&record)?;
                let mut installed = record;
                installed.state = ArtifactInventoryStateV1::Installed;
                installed.quarantine_reason = None;
                let mut inventory = self.load_inventory_locked()?;
                inventory
                    .records
                    .insert(installed.artifact_digest.to_string(), installed);
                self.save_inventory_locked(&inventory)?;
                if staging_exists {
                    self.remove_staging_dir_path(staging_id)?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut inventory = self.load_inventory_locked()?;
                if staging_exists {
                    inventory
                        .records
                        .insert(record.artifact_digest.to_string(), record);
                } else {
                    let mut quarantined = record;
                    quarantined.state = ArtifactInventoryStateV1::Quarantined;
                    quarantined.quarantine_reason = Some(QuarantineReasonV1::RecoveryFailure);
                    inventory
                        .records
                        .insert(quarantined.artifact_digest.to_string(), quarantined);
                }
                self.save_inventory_locked(&inventory)?;
            }
            Err(_) => return Err(ArtifactImportErrorV1::StorageFailure),
        }
        Ok(())
    }

    pub(super) fn recover_gc_locked(
        &self,
        records: Vec<ArtifactInventoryRecordV1>,
        recorded_at_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let mut inventory = self.load_inventory_locked()?;
        for record in &records {
            self.remove_artifact_record(record)?;
            inventory
                .records
                .remove(&record.artifact_digest.to_string());
        }
        self.save_inventory_locked(&inventory)?;
        let receipts = records
            .into_iter()
            .map(|record| GcReceiptV1 {
                artifact_digest: record.artifact_digest,
                removed_at_unix: recorded_at_unix,
                prior_state: record.state,
            })
            .collect::<Vec<_>>();
        self.append_receipts_locked(&receipts)
    }

    pub(super) fn recover_staged_imports_locked(&self) -> Result<(), ArtifactImportErrorV1> {
        self.recover_staged_ids_locked(self.staged_ids_locked()?)
    }

    pub(super) fn staged_ids_locked(&self) -> Result<Vec<String>, ArtifactImportErrorV1> {
        let entries = self
            .staging_dir
            .entries()
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        let mut staging_ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            let file_type = entry
                .file_type()
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let Some(staging_id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_valid_staging_id(&staging_id) {
                continue;
            }
            staging_ids.push(staging_id);
        }
        Ok(staging_ids)
    }

    pub(super) fn recover_staged_ids_locked(
        &self,
        staging_ids: Vec<String>,
    ) -> Result<(), ArtifactImportErrorV1> {
        for staging_id in staging_ids {
            let Ok(staging_dir) = self.staging_dir.open_dir_nofollow(&staging_id) else {
                continue;
            };
            let members_dir = staging_dir.open_dir_nofollow("members").ok();
            let meta = match read_staging_meta(&staging_dir) {
                Ok(meta) if meta.schema == STAGING_SCHEMA_V1 => meta,
                Ok(_) | Err(_) => continue,
            };
            if self.verify_manifest(&meta.manifest).is_err() {
                continue;
            }
            let mut record = self.record_for(
                &meta.manifest,
                ArtifactInventoryStateV1::Staged,
                meta.verified_at_unix,
                None,
            );
            let mut inventory = self.load_inventory_locked()?;
            let existing_state = inventory
                .records
                .get(&record.artifact_digest.to_string())
                .map(|record| record.state);
            if matches!(
                existing_state,
                Some(ArtifactInventoryStateV1::Quarantined | ArtifactInventoryStateV1::Revoked)
            ) {
                drop(members_dir);
                drop(staging_dir);
                self.remove_staging_dir_path(&staging_id)?;
            } else if self
                .artifacts_dir
                .symlink_metadata(record.artifact_digest.as_str())
                .is_ok()
                && self.verify_artifact_record(&record).is_ok()
            {
                record.state = ArtifactInventoryStateV1::Installed;
                inventory
                    .records
                    .insert(record.artifact_digest.to_string(), record);
                self.save_inventory_locked(&inventory)?;
                drop(members_dir);
                drop(staging_dir);
                self.remove_staging_dir_path(&staging_id)?;
            } else {
                if members_dir.is_none() {
                    record.state = ArtifactInventoryStateV1::Quarantined;
                    record.quarantine_reason = Some(QuarantineReasonV1::RecoveryFailure);
                }
                inventory
                    .records
                    .insert(record.artifact_digest.to_string(), record);
                self.save_inventory_locked(&inventory)?;
            }
        }
        Ok(())
    }

    pub(super) fn verify_artifact_record(
        &self,
        record: &ArtifactInventoryRecordV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        let directory = self
            .artifacts_dir
            .open_dir_nofollow(record.artifact_digest.as_str())
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        for member in &record.members {
            let file = open_cap_file(
                &directory,
                member_file_name(member.role),
                true,
                false,
                false,
                false,
                false,
            )?;
            let metadata = file
                .metadata()
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            if metadata.len() != member.byte_length || sha256_open_file(file)? != member.digest {
                return Err(ArtifactImportErrorV1::DigestMismatch);
            }
        }
        Ok(())
    }

    pub(super) fn remove_artifact_record(
        &self,
        record: &ArtifactInventoryRecordV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        match self
            .artifacts_dir
            .symlink_metadata(record.artifact_digest.as_str())
        {
            Ok(metadata) if metadata.is_dir() => self
                .artifacts_dir
                .remove_dir_all(record.artifact_digest.as_str())?,
            Ok(_) => return Err(ArtifactImportErrorV1::UnsafeStorePath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ArtifactImportErrorV1::StorageFailure),
        }
        sync_cap_dir(&self.artifacts_dir)?;
        Ok(())
    }

    pub(super) fn remove_staging_dir_path(&self, staging_id: &str) -> Result<(), ArtifactImportErrorV1> {
        if !is_valid_staging_id(staging_id) {
            return Err(ArtifactImportErrorV1::UnsafeStagingHandle);
        }
        match self.staging_dir.symlink_metadata(staging_id) {
            Ok(metadata) if metadata.is_dir() => self.staging_dir.remove_dir_all(staging_id)?,
            Ok(_) => self.staging_dir.remove_file_or_symlink(staging_id)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ArtifactImportErrorV1::StorageFailure),
        }
        sync_cap_dir(&self.staging_dir)?;
        Ok(())
    }

    pub(super) fn append_receipts_locked(
        &self,
        receipts: &[GcReceiptV1],
    ) -> Result<(), ArtifactImportErrorV1> {
        if receipts.is_empty() {
            return Ok(());
        }
        let mut durable = read_receipt_frames(
            read_optional_cap_file(&self.receipts_dir, "gc.jsonl")?
                .as_deref()
                .unwrap_or_default(),
        )?;
        for receipt in receipts {
            if !durable.contains(receipt) {
                durable.push(receipt.clone());
            }
        }
        let mut bytes = Vec::new();
        for receipt in &durable {
            serde_json::to_writer(&mut bytes, receipt)
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            bytes.push(b'\n');
        }
        atomic_write_cap_file(
            &self.receipts_dir,
            &self.receipts_root(),
            "gc.jsonl",
            &bytes,
        )
    }
}
