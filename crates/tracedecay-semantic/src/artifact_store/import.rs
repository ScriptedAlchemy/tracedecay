//! Resumable import, staging, and revoke/rollback-retain flows for
//! ModelArtifactStore. Split out of artifact_store.rs to keep the facade
//! under the 1000-line hygiene ceiling; pure structural move, no behavior
//! change.

use super::*;

impl ModelArtifactStore {
    /// Begin a resumable import of caller-provided bytes for a verified
    /// manifest. Stages under a random local directory; no network access.
    pub fn begin_import(
        &self,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<ImportSession, ArtifactImportErrorV1> {
        self.verify_manifest(manifest)?;
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        if self
            .load_inventory_locked()?
            .records
            .contains_key(&manifest.artifact_identity_digest().to_string())
        {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        let (staging_id, staging_dir) = (0..16)
            .find_map(|_| {
                let staging_id = random_staging_id().ok()?;
                #[allow(unused_mut)] // mode() is unix-only
                let mut builder = DirBuilder::new();
                #[cfg(unix)]
                builder.mode(0o700);
                match self.staging_dir.create_dir_with(&staging_id, &builder) {
                    Ok(()) => {
                        let staging_dir = self.staging_dir.open_dir_nofollow(&staging_id).ok()?;
                        Some((staging_id, staging_dir))
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(_) => None,
                }
            })
            .ok_or(ArtifactImportErrorV1::StorageFailure)?;
        #[allow(unused_mut)] // mode() is unix-only
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        staging_dir
            .create_dir_with("members", &builder)
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        let members_dir = staging_dir
            .open_dir_nofollow("members")
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        let meta = StagingMetaV1 {
            schema: STAGING_SCHEMA_V1.to_string(),
            manifest: manifest.clone(),
            manifest_identity_digest: manifest.artifact_identity_digest(),
            verified_at_unix: now_unix,
            immutable_source_revision: None,
            members: manifest
                .payload
                .members
                .iter()
                .cloned()
                .map(|member| StagedMemberV1 {
                    member,
                    bytes_written: 0,
                })
                .collect(),
        };
        for member in &meta.members {
            let _file = open_cap_file(
                &members_dir,
                member_file_name(member.member.role),
                false,
                true,
                false,
                true,
                false,
            )?;
        }
        write_staging_meta(&staging_dir, &self.staging_dir_for(&staging_id)?, &meta)?;
        let record = self.record_for(manifest, ArtifactInventoryStateV1::Staged, now_unix, None);
        let mut inventory = self.load_inventory_locked()?;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record);
        self.save_inventory_locked(&inventory)?;
        sync_cap_dir(&self.staging_dir)?;
        Ok(ImportSession {
            staging_path: self.staging_dir_for(&staging_id)?,
            staging_id,
            staging_dir,
            members_dir,
            meta,
        })
    }

    /// Resume an interrupted import. Permitted only because the manifest pins
    /// immutable length and digest identity; a sidecar mismatch discards the
    /// staging directory and reports a typed error.
    pub fn resume_import(
        &self,
        manifest: &ModelArtifactManifestV1,
        staging_id: &str,
        now_unix: u64,
    ) -> Result<ImportSession, ArtifactImportErrorV1> {
        let staging_path = self.staging_dir_for(staging_id)?;
        self.verify_manifest(manifest)?;
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let staging_dir = self
            .staging_dir
            .open_dir_nofollow(staging_id)
            .map_err(|_| ArtifactImportErrorV1::StagingUnavailable)?;
        let members_dir = staging_dir
            .open_dir_nofollow("members")
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        let meta = read_staging_meta(&staging_dir)?;
        let session = ImportSession {
            staging_id: staging_id.to_string(),
            staging_path,
            staging_dir,
            members_dir,
            meta,
        };
        self.ensure_session_active_locked(&session)?;
        if !self.staging_meta_matches(&session.meta, manifest)
            || !self.staging_member_lengths_match(&session)?
        {
            self.quarantine_staging_locked(
                &session,
                QuarantineReasonV1::IdentityMismatch,
                now_unix,
            )?;
            let staging_id = session.staging_id.clone();
            drop(session);
            self.remove_staging_dir_path(&staging_id)?;
            return Err(ArtifactImportErrorV1::ResumeIdentityMismatch);
        }
        Ok(session)
    }

    /// Append caller-provided bytes to the staged payload. Writes beyond the
    /// declared length are rejected as size expansion and quarantine the
    /// staged bytes (recorded against the declared digest) without exposing
    /// them to runtime discovery.
    #[cfg(test)]
    pub fn stage_chunk(
        &self,
        session: &mut ImportSession,
        bytes: &[u8],
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        self.stage_member_chunk(session, ArtifactMemberRoleV1::Model, bytes, now_unix)
    }

    /// Append caller-provided bytes to one explicitly declared package member.
    /// The role selects a store-owned filename; a manifest path is identity
    /// metadata only and can never influence local traversal.
    pub fn stage_member_chunk(
        &self,
        session: &mut ImportSession,
        role: ArtifactMemberRoleV1,
        bytes: &[u8],
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        self.ensure_session_dir(session)?;
        self.ensure_session_active_locked(session)?;
        let member_index = session
            .meta
            .members
            .iter()
            .position(|member| member.member.role == role)
            .ok_or(ArtifactImportErrorV1::MemberMismatch)?;
        let member = &session.meta.members[member_index];
        let attempted = member.bytes_written.saturating_add(bytes.len() as u64);
        if attempted > member.member.byte_length {
            self.quarantine_staging_locked(session, QuarantineReasonV1::SizeExpansion, now_unix)?;
            return Err(ArtifactImportErrorV1::SizeExpansionBeyondDeclared);
        }
        let mut file = open_cap_file(
            &session.members_dir,
            member_file_name(role),
            false,
            true,
            false,
            false,
            true,
        )?;
        file.write_all(bytes)?;
        file.sync_all()?;
        session.meta.members[member_index].bytes_written = attempted;
        write_staging_meta(&session.staging_dir, &session.staging_path, &session.meta)?;
        Ok(())
    }

    /// Import one explicit local directory. The directory must contain exactly
    /// the manifest members and only regular, single-link files. Paths are
    /// validated before any package bytes become runtime-discoverable.
    pub fn import_local_directory(
        &self,
        manifest: &ModelArtifactManifestV1,
        source: &Path,
        now_unix: u64,
    ) -> Result<ArtifactInventoryRecordV1, ArtifactImportErrorV1> {
        let mut session = self.begin_import(manifest, now_unix)?;
        let files = match inspect_local_package(source) {
            Ok(files) => files,
            Err(error) => {
                self.quarantine_and_discard(
                    &session,
                    quarantine_reason_for_import_error(&error),
                    now_unix,
                )?;
                return Err(error);
            }
        };
        let declared: BTreeMap<&str, &ArtifactPackageMemberV1> = manifest
            .payload
            .members
            .iter()
            .map(|member| (member.path.as_str(), member))
            .collect();
        if files
            .keys()
            .any(|path| !declared.contains_key(path.as_str()))
        {
            self.quarantine_and_discard(&session, QuarantineReasonV1::UndeclaredMember, now_unix)?;
            return Err(ArtifactImportErrorV1::UndeclaredMember);
        }
        if files.len() != declared.len() {
            self.quarantine_and_discard(&session, QuarantineReasonV1::IdentityMismatch, now_unix)?;
            return Err(ArtifactImportErrorV1::MemberMismatch);
        }

        for member in &manifest.payload.members {
            let path = files
                .get(&member.path)
                .ok_or(ArtifactImportErrorV1::MemberMismatch)?;
            let result = stream_local_member(self, &mut session, member, path, now_unix);
            if let Err(error) = result {
                self.quarantine_and_discard(
                    &session,
                    quarantine_reason_for_import_error(&error),
                    now_unix,
                )?;
                return Err(error);
            }
        }
        self.finalize_import(session, manifest, now_unix)
    }

    /// Import from an explicit immutable HTTPS source. Callers may pass a
    /// prior opaque staging handle only after an `InterruptedResumable`
    /// result. Each response must repeat the configured immutable revision,
    /// exact offset, and declared total length.
    pub fn import_configured_https(
        &self,
        manifest: &ModelArtifactManifestV1,
        source: &ConfiguredHttpsArtifactSourceV1,
        transport: &dyn ExplicitHttpsArtifactTransportV1,
        resume_staging_id: Option<&str>,
        now_unix: u64,
    ) -> Result<ArtifactInventoryRecordV1, ArtifactImportErrorV1> {
        let mut session = match resume_staging_id {
            Some(staging_id) => self.resume_import(manifest, staging_id, now_unix)?,
            None => self.begin_import(manifest, now_unix)?,
        };
        if let Some(pinned) = &session.meta.immutable_source_revision {
            if pinned != &source.immutable_revision {
                self.quarantine_and_discard(
                    &session,
                    QuarantineReasonV1::IdentityMismatch,
                    now_unix,
                )?;
                return Err(ArtifactImportErrorV1::ResumeIdentityMismatch);
            }
        } else {
            session.meta.immutable_source_revision = Some(source.immutable_revision.clone());
            write_staging_meta(&session.staging_dir, &session.staging_path, &session.meta)?;
        }

        for member_index in 0..session.meta.members.len() {
            let member = session.meta.members[member_index].member.clone();
            while session.meta.members[member_index].bytes_written < member.byte_length {
                let offset = session.meta.members[member_index].bytes_written;
                let request = HttpsArtifactRangeRequestV1 {
                    url: source.member_url(&member),
                    offset,
                    max_bytes: (member.byte_length - offset).min(64 * 1024),
                    expected_total_length: member.byte_length,
                    expected_sha256: member.digest.clone(),
                    immutable_revision: source.immutable_revision.clone(),
                };
                let response = match transport.fetch_range(&request) {
                    Ok(response) => response,
                    Err(_) => {
                        return Err(ArtifactImportErrorV1::InterruptedResumable {
                            staging_id: session.staging_id(),
                        });
                    }
                };
                let response_len = u64::try_from(response.bytes.len())
                    .map_err(|_| ArtifactImportErrorV1::SizeExpansionBeyondDeclared)?;
                if response.offset != offset
                    || response.total_length != member.byte_length
                    || response.immutable_revision != source.immutable_revision
                    || response.bytes.is_empty()
                    || response_len > request.max_bytes
                {
                    self.quarantine_and_discard(
                        &session,
                        QuarantineReasonV1::IdentityMismatch,
                        now_unix,
                    )?;
                    return Err(ArtifactImportErrorV1::ImmutableRangeMismatch);
                }
                if let Err(error) =
                    self.stage_member_chunk(&mut session, member.role, &response.bytes, now_unix)
                {
                    self.quarantine_and_discard(
                        &session,
                        quarantine_reason_for_import_error(&error),
                        now_unix,
                    )?;
                    return Err(error);
                }
            }
        }
        self.finalize_import(session, manifest, now_unix)
    }

    /// Finalize: stream length + SHA-256 verification of the staged bytes,
    /// fsync, atomic rename into the digest-addressed layout, fsync the
    /// directory, then publish the `Installed` inventory record. Digest or
    /// length mismatch quarantines the import.
    pub fn finalize_import(
        &self,
        session: ImportSession,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<ArtifactInventoryRecordV1, ArtifactImportErrorV1> {
        self.verify_manifest(manifest)?;
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        self.ensure_session_dir(&session)?;
        self.ensure_session_active_locked(&session)?;
        if !self.staging_meta_matches(&session.meta, manifest) {
            self.quarantine_staging_locked(
                &session,
                QuarantineReasonV1::IdentityMismatch,
                now_unix,
            )?;
            return Err(ArtifactImportErrorV1::ResumeIdentityMismatch);
        }

        for staged in &session.meta.members {
            let file = open_cap_file(
                &session.members_dir,
                member_file_name(staged.member.role),
                true,
                false,
                false,
                false,
                false,
            )?;
            let length = file
                .metadata()
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?
                .len();
            if length != staged.member.byte_length || staged.bytes_written != length {
                self.quarantine_staging_locked(
                    &session,
                    QuarantineReasonV1::MemberLengthMismatch,
                    now_unix,
                )?;
                return Err(ArtifactImportErrorV1::LengthMismatch);
            }
            let actual = sha256_open_file(file)?;
            if actual != staged.member.digest {
                self.quarantine_staging_locked(
                    &session,
                    QuarantineReasonV1::MemberDigestMismatch,
                    now_unix,
                )?;
                return Err(ArtifactImportErrorV1::DigestMismatch);
            }
        }

        let mut record =
            self.record_for(manifest, ArtifactInventoryStateV1::Verified, now_unix, None);
        let mut inventory = self.load_inventory_locked()?;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record.clone());
        self.save_inventory_locked(&inventory)?;
        self.write_recovery_locked(&RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Install {
                record: Box::new(record.clone()),
                staging_id: session.staging_id.clone(),
            },
        })?;

        let ImportSession {
            staging_id,
            staging_path: _,
            staging_dir,
            members_dir,
            meta: _,
        } = session;
        drop(members_dir);
        let destination = record.artifact_digest.as_str();
        match self.artifacts_dir.symlink_metadata(destination) {
            Ok(_) => self.verify_artifact_record(&record)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                staging_dir.rename("members", &self.artifacts_dir, destination)?;
                sync_cap_dir(&staging_dir)?;
                sync_cap_dir(&self.artifacts_dir)?;
            }
            Err(_) => return Err(ArtifactImportErrorV1::StorageFailure),
        }

        record.state = ArtifactInventoryStateV1::Installed;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record.clone());
        self.save_inventory_locked(&inventory)?;
        self.remove_staging_dir_path(&staging_id)?;
        self.clear_recovery_locked()?;
        Ok(record)
    }

    pub(super) fn quarantine_staging_locked(
        &self,
        session: &ImportSession,
        reason: QuarantineReasonV1,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        self.ensure_session_dir(session)?;
        let record = self.record_for(
            &session.meta.manifest,
            ArtifactInventoryStateV1::Quarantined,
            now_unix,
            Some(reason),
        );
        let mut inventory = self.load_inventory_locked()?;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record);
        self.save_inventory_locked(&inventory)
    }

    pub(super) fn quarantine_and_discard(
        &self,
        session: &ImportSession,
        reason: QuarantineReasonV1,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        {
            let _lock = self.acquire_lock()?;
            self.recover_locked()?;
            self.quarantine_staging_locked(session, reason, now_unix)?;
        }
        self.remove_staging_dir_path(&session.staging_id)
    }

    /// Mark an installed artifact revoked. Revoked artifacts are never
    /// admitted and are protected from GC (revocation evidence is retained).
    #[cfg(test)]
    pub fn revoke_artifact(
        &self,
        digest: &Sha256DigestHex,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        if let Some(record) = inventory.records.get_mut(&digest.to_string()) {
            record.state = ArtifactInventoryStateV1::Revoked;
            record.recorded_at_unix = now_unix;
        }
        self.save_inventory_locked(&inventory)
    }

    /// Retain an installed artifact explicitly for rollback; retained
    /// artifacts are never collected.
    #[cfg(test)]
    pub fn retain_for_rollback(
        &self,
        digest: &Sha256DigestHex,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        if let Some(record) = inventory.records.get_mut(&digest.to_string())
            && record.state == ArtifactInventoryStateV1::Installed
        {
            record.state = ArtifactInventoryStateV1::RetainedForRollback;
            record.recorded_at_unix = now_unix;
        }
        self.save_inventory_locked(&inventory)
    }
}
