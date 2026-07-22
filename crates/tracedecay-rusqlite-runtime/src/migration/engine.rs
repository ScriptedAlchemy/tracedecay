use std::collections::{BTreeMap, BTreeSet};

use super::{model::*, ports::*};
use tracedecay_store::StoreShardScopeV1;

pub(crate) struct ConsolidatedMigrationEngine<P> {
    port: P,
    last_released: LastReleasedSchemaManifest,
    final_schema: FinalSchemaManifest,
}

impl<P: ConsolidatedMigrationPort> ConsolidatedMigrationEngine<P> {
    pub(crate) fn new(
        port: P,
        last_released: LastReleasedSchemaManifest,
        final_schema: FinalSchemaManifest,
    ) -> Result<Self, MigrationError> {
        if last_released.manifest().canonical_digest == final_schema.manifest().canonical_digest
            || last_released.manifest().revision >= final_schema.manifest().revision
            || last_released
                .manifest()
                .families
                .keys()
                .collect::<BTreeSet<_>>()
                != final_schema
                    .manifest()
                    .families
                    .keys()
                    .collect::<BTreeSet<_>>()
        {
            return Err(MigrationError::InvalidContract("schema transition"));
        }
        Ok(Self {
            port,
            last_released,
            final_schema,
        })
    }

    pub(crate) fn into_port(self) -> P {
        self.port
    }

    pub(crate) fn migrate<T: FamilyTransform<Error = P::Error>>(
        &mut self,
        request: &MigrationRequest,
        transforms: &mut BTreeMap<StoreFamily, T>,
    ) -> Result<MigrationOutcome, MigrationError> {
        self.validate_request(request)?;
        if let Some(receipt) = self.call(MigrationStage::Publication, |port| {
            port.lookup_publication(&request.migration_id)
        })? {
            self.validate_receipt(request, &receipt, None)?;
            return Ok(MigrationOutcome::Replayed(receipt));
        }

        let accepted = self.call(MigrationStage::FreezeProof, |port| {
            port.verify_release_freeze(&request.freeze_proof)
        })?;
        if !accepted {
            return Err(MigrationError::FreezeProofRejected);
        }

        let preflight = self.call(MigrationStage::Preflight, |port| port.preflight(request))?;
        match self.validate_preflight(request, &preflight)? {
            PreflightDisposition::AlreadyFinal => return Ok(MigrationOutcome::AlreadyFinal),
            PreflightDisposition::Migrate => {}
        }

        let (staging, mut checkpoint) = self.resume_or_stage(request)?;
        self.validate_checkpoint(request, &staging, &checkpoint)?;
        let resumed_verified = matches!(checkpoint, DestinationCheckpoint::Verified { .. });
        if !resumed_verified {
            let mut completed = checkpoint
                .completed()
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            for (family, source_bindings) in &request.source_bindings {
                if completed.contains(family) {
                    continue;
                }
                let transform = transforms
                    .get_mut(family)
                    .ok_or(MigrationError::InvalidContract("missing family transform"))?;
                if transform.family() != *family {
                    return Err(MigrationError::InvalidContract("misbound family transform"));
                }
                let target = self
                    .final_schema
                    .manifest()
                    .families
                    .get(family)
                    .ok_or(MigrationError::UnknownFamily(*family))?;
                let source_manifest = self
                    .last_released
                    .manifest()
                    .families
                    .get(family)
                    .ok_or(MigrationError::UnknownFamily(*family))?;
                let plan = FamilyTransformPlan {
                    family: *family,
                    source_bindings: source_bindings.clone(),
                    source_schema_digest: source_manifest.schema_digest,
                    destination_schema_digest: target.schema_digest,
                    transform_revision: target.transform_revision,
                };
                let mut next = completed.iter().copied().collect::<Vec<_>>();
                next.push(*family);
                next.sort();
                checkpoint = self.call(MigrationStage::Transform(*family), |port| {
                    port.transform_and_checkpoint(&staging, transform, &plan, &next)
                })?;
                self.validate_checkpoint(request, &staging, &checkpoint)?;
                completed = checkpoint.completed().iter().copied().collect();
                if !completed.contains(family) {
                    return Err(MigrationError::CheckpointMismatch);
                }
            }
        }

        let last_released = self.last_released.clone();
        let final_schema = self.final_schema.clone();
        let report = self.call(MigrationStage::Verification, |port| {
            port.verify_staging(&staging, &last_released, &final_schema)
        })?;
        self.validate_verification(&report)?;
        if resumed_verified {
            match &checkpoint {
                DestinationCheckpoint::Verified {
                    verification_digest,
                    ..
                } if *verification_digest == report.integrity_check_digest => {}
                _ => return Err(MigrationError::CheckpointMismatch),
            }
        } else {
            let backup = checkpoint.backup().clone();
            checkpoint = self.call(MigrationStage::Verification, |port| {
                port.checkpoint_verification(&staging, &backup, &report)
            })?;
            self.validate_checkpoint(request, &staging, &checkpoint)?;
            match &checkpoint {
                DestinationCheckpoint::Verified {
                    verification_digest,
                    ..
                } if *verification_digest == report.integrity_check_digest => {}
                _ => return Err(MigrationError::CheckpointMismatch),
            }
        }

        let final_schema = self.final_schema.clone();
        let receipt = self
            .call(MigrationStage::Publication, |port| {
                port.publish_atomically(staging, request, &final_schema, &checkpoint)
            })
            .or_else(|error| {
                let replay = self.call(MigrationStage::Publication, |port| {
                    port.lookup_publication(&request.migration_id)
                })?;
                replay.ok_or(error)
            })?;
        self.validate_receipt(request, &receipt, Some(&checkpoint))?;
        Ok(MigrationOutcome::Published(receipt))
    }

    fn resume_or_stage(
        &mut self,
        request: &MigrationRequest,
    ) -> Result<(StagingHandle, DestinationCheckpoint), MigrationError> {
        if let Some(staging) = self.call(MigrationStage::Staging, |port| {
            port.find_staging(&request.migration_id)
        })? {
            let checkpoint = self.call(MigrationStage::Staging, |port| {
                port.load_destination_checkpoint(&staging)
            })?;
            return Ok((staging, checkpoint));
        }
        let last_released = self.last_released.clone();
        let backup = self.call(MigrationStage::Backup, |port| {
            port.create_isolated_backup(request, &last_released)
        })?;
        if backup.source_manifest_digest != self.last_released.manifest().canonical_digest {
            return Err(MigrationError::CheckpointMismatch);
        }
        let final_schema = self.final_schema.clone();
        let staging = self.call(MigrationStage::Staging, |port| {
            port.create_isolated_staging(request, &final_schema, &backup)
        })?;
        let checkpoint = self.call(MigrationStage::Staging, |port| {
            port.load_destination_checkpoint(&staging)
        })?;
        Ok((staging, checkpoint))
    }

    fn validate_request(&self, request: &MigrationRequest) -> Result<(), MigrationError> {
        if request.freeze_proof.last_released_digest
            != self.last_released.manifest().canonical_digest
            || request.freeze_proof.final_digest != self.final_schema.manifest().canonical_digest
            || request.source_bindings.keys().collect::<BTreeSet<_>>()
                != self
                    .last_released
                    .manifest()
                    .families
                    .keys()
                    .collect::<BTreeSet<_>>()
        {
            return Err(MigrationError::FreezeProofRejected);
        }
        if request
            .source_bindings
            .values()
            .flatten()
            .any(|binding| request.destination_epoch.get() <= binding.authority_epoch.get())
        {
            return Err(MigrationError::StaleDestinationEpoch);
        }
        let mut authority_root = None;
        let mut project_id = None;
        let mut shard_ids = BTreeSet::new();
        for (family, bindings) in &request.source_bindings {
            if bindings.is_empty() {
                return Err(MigrationError::BindingMismatch(*family));
            }
            for binding in bindings {
                let root = (&binding.shard_id.brain_id, &binding.shard_id.profile_id);
                match authority_root {
                    None => authority_root = Some(root),
                    Some(expected) if expected == root => {}
                    Some(_) => return Err(MigrationError::BindingMismatch(*family)),
                }
                if !shard_ids.insert(&binding.shard_id) {
                    return Err(MigrationError::BindingMismatch(*family));
                }
                if let Some(binding_project_id) = binding.shard_id.scope.project_id() {
                    match project_id {
                        None => project_id = Some(binding_project_id),
                        Some(expected) if expected == binding_project_id => {}
                        Some(_) => return Err(MigrationError::BindingMismatch(*family)),
                    }
                }
                let canonical = matches!(
                    (family, &binding.shard_id.scope),
                    (StoreFamily::Profile, StoreShardScopeV1::Profile)
                        | (StoreFamily::Project, StoreShardScopeV1::Project { .. })
                        | (
                            StoreFamily::ProjectSessions,
                            StoreShardScopeV1::ProjectSessions { .. }
                        )
                        | (StoreFamily::Code, StoreShardScopeV1::Code { .. })
                );
                if !canonical {
                    return Err(MigrationError::BindingMismatch(*family));
                }
            }
        }
        Ok(())
    }

    fn validate_preflight(
        &self,
        request: &MigrationRequest,
        report: &PreflightReport,
    ) -> Result<PreflightDisposition, MigrationError> {
        if report.families.keys().collect::<BTreeSet<_>>()
            != request.source_bindings.keys().collect::<BTreeSet<_>>()
        {
            return Err(MigrationError::InvalidContract("incomplete preflight"));
        }
        let mut final_count = 0;
        for (family, result) in &report.families {
            if result.family != *family
                || request.source_bindings.get(family) != Some(&result.bindings)
            {
                return Err(MigrationError::BindingMismatch(*family));
            }
            match result.observed_schema {
                ObservedSchema::LastReleased(digest)
                    if digest == self.last_released.manifest().families[family].schema_digest => {}
                ObservedSchema::Final(digest)
                    if digest == self.final_schema.manifest().families[family].schema_digest =>
                {
                    final_count += 1;
                }
                ObservedSchema::Unknown { .. } => {
                    return Err(MigrationError::UnknownFamily(*family));
                }
                ObservedSchema::Corrupt => return Err(MigrationError::CorruptFamily(*family)),
                _ => return Err(MigrationError::SchemaMismatch(*family)),
            }
        }
        if final_count == report.families.len() {
            Ok(PreflightDisposition::AlreadyFinal)
        } else if final_count == 0 {
            Ok(PreflightDisposition::Migrate)
        } else {
            Err(MigrationError::InvalidContract("partially migrated family"))
        }
    }

    fn validate_checkpoint(
        &self,
        request: &MigrationRequest,
        staging: &StagingHandle,
        checkpoint: &DestinationCheckpoint,
    ) -> Result<(), MigrationError> {
        if staging.migration_id != request.migration_id
            || staging.destination_epoch != request.destination_epoch
            || checkpoint.migration_id() != &request.migration_id
            || checkpoint.migration_id() != &staging.migration_id
            || checkpoint.staging_id() != &staging.staging_id
            || checkpoint.destination_epoch() != request.destination_epoch
            || checkpoint.destination_epoch() != staging.destination_epoch
            || checkpoint.backup().source_manifest_digest
                != self.last_released.manifest().canonical_digest
            || checkpoint.final_manifest_digest() != self.final_schema.manifest().canonical_digest
        {
            return Err(MigrationError::CheckpointMismatch);
        }
        let completed = checkpoint.completed();
        if completed.windows(2).any(|pair| pair[0] >= pair[1])
            || completed
                .iter()
                .any(|family| !request.source_bindings.contains_key(family))
        {
            return Err(MigrationError::CheckpointMismatch);
        }
        Ok(())
    }

    fn validate_verification(&self, report: &VerificationReport) -> Result<(), MigrationError> {
        if report.destination_manifest_digest != self.final_schema.manifest().canonical_digest
            || report.families.keys().collect::<BTreeSet<_>>()
                != self
                    .final_schema
                    .manifest()
                    .families
                    .keys()
                    .collect::<BTreeSet<_>>()
        {
            return Err(MigrationError::CheckpointMismatch);
        }
        for (family, verification) in &report.families {
            if !verification.is_exact() {
                return Err(MigrationError::VerificationFailed(*family));
            }
        }
        Ok(())
    }

    fn validate_receipt(
        &self,
        request: &MigrationRequest,
        receipt: &PublicationReceipt,
        checkpoint: Option<&DestinationCheckpoint>,
    ) -> Result<(), MigrationError> {
        if receipt.migration_id != request.migration_id
            || receipt.final_manifest_digest != self.final_schema.manifest().canonical_digest
            || receipt.destination_epoch != request.destination_epoch
            || receipt.backup.source_manifest_digest
                != self.last_released.manifest().canonical_digest
        {
            return Err(MigrationError::CheckpointMismatch);
        }
        if let Some(checkpoint) = checkpoint {
            match checkpoint {
                DestinationCheckpoint::Verified {
                    verification_digest,
                    ..
                } if receipt.verification_digest == *verification_digest
                    && &receipt.backup == checkpoint.backup() => {}
                _ => return Err(MigrationError::CheckpointMismatch),
            }
        }
        Ok(())
    }

    fn call<R>(
        &mut self,
        stage: MigrationStage,
        operation: impl FnOnce(&mut P) -> Result<R, P::Error>,
    ) -> Result<R, MigrationError> {
        operation(&mut self.port).map_err(|error| MigrationError::Port {
            stage,
            detail: error.to_string(),
        })
    }
}

enum PreflightDisposition {
    Migrate,
    AlreadyFinal,
}
