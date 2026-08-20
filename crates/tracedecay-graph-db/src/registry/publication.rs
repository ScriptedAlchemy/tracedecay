use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_store::runtime::{
    GraphProjectionIdentityV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationProjectionPageRequestV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayPageRequestV1, GraphPublicationReplayRetirementV1,
    GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationStoreV1,
    GraphReplayRetirementOutcomeV1, GraphRetiredReplayCleanupFinalizeOutcomeV1,
    GraphVerifiedHeadCasOutcomeV1, GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1,
    MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1, MAX_GRAPH_REPLAY_PAGE_RECORDS_V1,
};

use super::publication_support::{
    RegisteredGraphDbOperationV1, check_all, clear_retiring_fence, collect_closure,
    dependency_key_for_binding, locator_from_dependency, locator_from_key, map_publication_error,
    require_active_replay_evidence, require_head_replay, require_projection_binding,
    retain_lease_closure, validate_exact_dependency_closure, validate_replay_cursor,
};
use super::{GraphDbRegistration, GraphDbRegistry, check_registration_request};
use crate::generation::{
    metadata_manifest_from_replay, validate_supplied_manifest_binding, verify_recovered_generation,
};
use crate::lease::{
    GenerationLocator, VerifiedGenerationLease, VerifiedGraphSnapshot, generation_lease,
};
use crate::{
    GraphDb, GraphDbError, GraphDbLeaseV1, GraphGenerationManifest, GraphGenerationReplaySource,
    GraphProjectionIdentity, GraphReplayCollectionOutcome, VerifiedGraphCommit,
};

impl GraphDbRegistry {
    pub fn retire_one_code_generation_replay(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        generation: &tracedecay_domain::CodeGenerationId,
        sealed_state_digest: &crate::SealedGraphStateDigest,
    ) -> Result<GraphReplayCollectionOutcome, GraphDbError> {
        check_all(&registration, context)?;
        let database = self.resolve(registration.clone())?;
        let mut projections = Vec::new();
        let mut after = None;
        loop {
            let request = GraphPublicationProjectionPageRequestV1::new(
                registration.binding().shard_id.clone(),
                after.clone(),
                MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1,
            )
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
            let page = authority
                .projection_page(&request, context)
                .map_err(map_publication_error)?;
            for projection in &page.projections {
                require_projection_binding(&registration, projection)?;
            }
            projections.extend(page.projections);
            let Some(continuation) = page.continuation else {
                break;
            };
            if after
                .as_ref()
                .is_some_and(|previous| continuation <= *previous)
            {
                return Err(GraphDbError::Corrupt {
                    message: "relational graph projection cursor did not advance".to_owned(),
                });
            }
            after = Some(continuation);
        }

        let mut retained = BTreeSet::new();
        let mut candidates = Vec::new();
        let mut retired_cleanup = Vec::new();
        let mut sealed_digest_mismatch = false;
        for projection in projections {
            if let Some(head) = authority
                .verified_head(&projection, context)
                .map_err(map_publication_error)?
            {
                retained.insert(locator_from_key(&head.key)?);
            }
            if let Some(pending) = authority
                .pending_replay(&projection, context)
                .map_err(map_publication_error)?
            {
                retained.insert(locator_from_key(&pending.publication.key)?);
            }
            let mut replay_after = None;
            loop {
                let request = GraphPublicationReplayPageRequestV1::new(
                    projection.clone(),
                    replay_after.clone(),
                    MAX_GRAPH_REPLAY_PAGE_RECORDS_V1,
                )
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
                let page = authority
                    .replay_page(&request, context)
                    .map_err(map_publication_error)?;
                for replay in page.records {
                    if replay.publication.key.projection != projection {
                        return Err(GraphDbError::Corrupt {
                            message: "relational graph replay page escaped its projection"
                                .to_owned(),
                        });
                    }
                    let owner = locator_from_key(&replay.publication.key)?;
                    for dependency in &replay.publication.direct_dependency_generations {
                        retained.insert(locator_from_dependency(&registration, dependency)?);
                    }
                    let source = crate::generation::checked_decode_replay_source(
                        &replay.publication.canonical_replay_source,
                        &|| check_all(&registration, context),
                    )?;
                    if let GraphGenerationReplaySource::SealedCodeGeneration(sealed) = &source
                        && &sealed.generation == generation
                    {
                        if &sealed.sealed_state_digest == sealed_state_digest {
                            candidates.push((owner, replay, source));
                        } else {
                            sealed_digest_mismatch = true;
                        }
                    }
                }
                let Some(continuation) = page.continuation else {
                    break;
                };
                validate_replay_cursor(
                    &projection,
                    replay_after.as_ref(),
                    &continuation,
                    "relational graph replay",
                )?;
                replay_after = Some(continuation);
            }
            let mut cleanup_after = None;
            loop {
                let request = GraphPublicationRetiredCleanupPageRequestV1::new(
                    projection.clone(),
                    cleanup_after.clone(),
                    MAX_GRAPH_REPLAY_PAGE_RECORDS_V1,
                )
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
                let page = authority
                    .retired_cleanup_page(&request, context)
                    .map_err(map_publication_error)?;
                for tombstone in page.records {
                    if tombstone.key.projection != projection {
                        return Err(GraphDbError::Corrupt {
                            message: "retired graph cleanup page escaped its projection".to_owned(),
                        });
                    }
                    let source_payload =
                        tombstone.canonical_replay_source.as_ref().ok_or_else(|| {
                            GraphDbError::Corrupt {
                                message: "retired graph cleanup lost its replay source".to_owned(),
                            }
                        })?;
                    let source =
                        crate::generation::checked_decode_replay_source(source_payload, &|| {
                            check_all(&registration, context)
                        })?;
                    if let GraphGenerationReplaySource::SealedCodeGeneration(sealed) = &source
                        && &sealed.generation == generation
                    {
                        if &sealed.sealed_state_digest == sealed_state_digest {
                            retired_cleanup.push((locator_from_key(&tombstone.key)?, tombstone));
                        } else {
                            sealed_digest_mismatch = true;
                        }
                    }
                }
                let Some(continuation) = page.continuation else {
                    break;
                };
                validate_replay_cursor(
                    &projection,
                    cleanup_after.as_ref(),
                    &continuation,
                    "retired graph cleanup",
                )?;
                cleanup_after = Some(continuation);
            }
        }
        if sealed_digest_mismatch {
            return Err(GraphDbError::Conflict);
        }
        if candidates.is_empty() {
            for (locator, _) in retired_cleanup {
                database.delete_generation_contents(&locator, &|| {
                    check_registration_request(&registration)
                })?;
            }
            return Ok(GraphReplayCollectionOutcome::Absent);
        }
        let selected = {
            let mut state = database.inner.verified_generations.write().map_err(|_| {
                GraphDbError::unavailable("verified graph generation state lock is poisoned")
            })?;
            for head in state.heads.values() {
                retain_lease_closure(head, &mut retained);
            }
            for (locator, weak) in &state.known {
                if weak.upgrade().is_some() {
                    retained.insert(locator.clone());
                }
            }
            let selected = candidates
                .into_iter()
                .find(|(locator, _, _)| !retained.contains(locator));
            if let Some((locator, _, _)) = &selected {
                state.retiring.insert(locator.clone());
            }
            selected
        };
        let Some((locator, replay, source)) = selected else {
            return Ok(GraphReplayCollectionOutcome::Retained);
        };
        let retirement = match GraphPublicationReplayRetirementV1::new(
            replay.publication.key.clone(),
            replay.publication.input_digest.clone(),
            replay
                .publication
                .dependency_generation_closure_digest
                .clone(),
            replay.publication.direct_dependency_generations.clone(),
            replay.publication.expected_prior_head.clone(),
            replay.publication.expected_recovered_digest.clone(),
            replay.publication.canonical_replay_source_digest.clone(),
        ) {
            Ok(retirement) => retirement,
            Err(error) => {
                clear_retiring_fence(&database, &locator)?;
                return Err(GraphDbError::invalid(error.to_string()));
            }
        };
        let retirement_outcome = match authority.retire_replay(&retirement, context) {
            Ok(outcome) => outcome,
            Err(error) => {
                clear_retiring_fence(&database, &locator)?;
                return Err(map_publication_error(error));
            }
        };
        match retirement_outcome {
            GraphReplayRetirementOutcomeV1::Retired(_)
            | GraphReplayRetirementOutcomeV1::ExactReplay(_) => {
                // Retirement is the linearization point. A failure after it
                // may leak derived bytes, but cannot destroy the source of an
                // active relational replay.
                if let Err(error) = database.delete_generation_contents(&locator, &|| {
                    check_registration_request(&registration)
                }) {
                    clear_retiring_fence(&database, &locator)?;
                    return Err(error);
                }
                Ok(GraphReplayCollectionOutcome::Retired(source))
            }
            GraphReplayRetirementOutcomeV1::CurrentVerifiedHead { .. }
            | GraphReplayRetirementOutcomeV1::PendingReplay { .. } => {
                clear_retiring_fence(&database, &locator)?;
                Ok(GraphReplayCollectionOutcome::Retained)
            }
            GraphReplayRetirementOutcomeV1::Conflict => {
                clear_retiring_fence(&database, &locator)?;
                Err(GraphDbError::Conflict)
            }
            GraphReplayRetirementOutcomeV1::Missing => {
                clear_retiring_fence(&database, &locator)?;
                Err(GraphDbError::Corrupt {
                    message: "graph replay disappeared during exact retirement".to_owned(),
                })
            }
        }
    }

    pub fn finalize_one_code_generation_replay_cleanup(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        generation: &tracedecay_domain::CodeGenerationId,
        sealed_state_digest: &crate::SealedGraphStateDigest,
    ) -> Result<bool, GraphDbError> {
        check_all(&registration, context)?;
        let mut projection_after = None;
        loop {
            let request = GraphPublicationProjectionPageRequestV1::new(
                registration.binding().shard_id.clone(),
                projection_after.clone(),
                MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1,
            )
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
            let page = authority
                .projection_page(&request, context)
                .map_err(map_publication_error)?;
            for projection in page.projections {
                require_projection_binding(&registration, &projection)?;
                let mut cleanup_after = None;
                loop {
                    let request = GraphPublicationRetiredCleanupPageRequestV1::new(
                        projection.clone(),
                        cleanup_after.clone(),
                        MAX_GRAPH_REPLAY_PAGE_RECORDS_V1,
                    )
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
                    let cleanup = authority
                        .retired_cleanup_page(&request, context)
                        .map_err(map_publication_error)?;
                    for tombstone in cleanup.records {
                        if tombstone.key.projection != projection {
                            return Err(GraphDbError::Corrupt {
                                message: "retired graph cleanup page escaped its projection"
                                    .to_owned(),
                            });
                        }
                        let payload =
                            tombstone.canonical_replay_source.as_ref().ok_or_else(|| {
                                GraphDbError::Corrupt {
                                    message: "retired graph cleanup lost its replay source"
                                        .to_owned(),
                                }
                            })?;
                        let source =
                            crate::generation::checked_decode_replay_source(payload, &|| {
                                check_all(&registration, context)
                            })?;
                        if let GraphGenerationReplaySource::SealedCodeGeneration(source) = source
                            && &source.generation == generation
                        {
                            if &source.sealed_state_digest != sealed_state_digest {
                                return Err(GraphDbError::Conflict);
                            }
                            return match authority
                                .finalize_retired_replay_cleanup(&tombstone.retirement(), context)
                                .map_err(map_publication_error)?
                            {
                                GraphRetiredReplayCleanupFinalizeOutcomeV1::Finalized(_)
                                | GraphRetiredReplayCleanupFinalizeOutcomeV1::ExactReplay(_) => {
                                    Ok(true)
                                }
                                GraphRetiredReplayCleanupFinalizeOutcomeV1::Conflict => {
                                    Err(GraphDbError::Conflict)
                                }
                                GraphRetiredReplayCleanupFinalizeOutcomeV1::Missing => {
                                    Err(GraphDbError::Corrupt {
                                        message:
                                            "retired graph cleanup disappeared before finalization"
                                                .to_owned(),
                                    })
                                }
                            };
                        }
                    }
                    let Some(continuation) = cleanup.continuation else {
                        break;
                    };
                    validate_replay_cursor(
                        &projection,
                        cleanup_after.as_ref(),
                        &continuation,
                        "retired graph cleanup",
                    )?;
                    cleanup_after = Some(continuation);
                }
            }
            let Some(continuation) = page.continuation else {
                return Ok(false);
            };
            require_projection_binding(&registration, &continuation)?;
            if projection_after
                .as_ref()
                .is_some_and(|previous| continuation <= *previous)
            {
                return Err(GraphDbError::Corrupt {
                    message: "relational graph projection cursor did not advance".to_owned(),
                });
            }
            projection_after = Some(continuation);
        }
    }

    /// Publishes the journaled replay behind `publication_key` through the
    /// one crash-safe first-publish protocol: apply the graph batch as
    /// unverified projection work, close and reopen the database file,
    /// recompute the recovered generation digest from actual rows, and only
    /// then advance the relational verified head by compare-and-swap. A
    /// successful WAL sync during the apply is not a publication receipt;
    /// nothing is served until the recovered digest matches.
    ///
    /// A supplied manifest carries the native rows already in the caller's
    /// hands (a sealed code generation's projection, or a semantic-vector
    /// manifest whose canonical source is metadata-only) so first publication
    /// does not re-read and re-project the canonical replay source. It is
    /// validated against the journaled replay binding before any row is
    /// applied; a foreign manifest for the same journaled replay conflicts.
    /// Without one, the manifest is reconstructed from the journaled
    /// canonical replay source.
    pub fn publish_verified(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
        supplied_manifest: Option<GraphGenerationManifest>,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        self.publish_verified_inner(
            &operation,
            authority,
            context,
            publication_key,
            supplied_manifest,
            false,
        )
    }

    /// Publishes through an already-issued, registry-validated graph lease.
    ///
    /// The caller retains the exact operation lease through the publication;
    /// this does not reconstruct a Store registration or mint a second graph
    /// client token.
    pub fn publish_verified_with_lease(
        &self,
        database: &GraphDbLeaseV1,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let operation = self.registered_operation_with_lease(database)?;
        self.publish_verified_inner(&operation, authority, context, publication_key, None, false)
    }

    pub(super) fn publish_ready_staged_generation(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        self.publish_verified_inner(&operation, authority, context, publication_key, None, true)
    }

    fn publish_verified_inner(
        &self,
        operation: &RegisteredGraphDbOperationV1,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
        supplied_manifest: Option<GraphGenerationManifest>,
        reopen_metadata: bool,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        operation.check(self, context)?;
        operation.require_publication_binding(publication_key)?;
        let database = operation.database().clone();
        let check = || operation.check(self, context);
        let replay = authority
            .replay(publication_key, context)
            .map_err(map_publication_error)?;
        let replay = match replay {
            GraphPublicationReplayLookupV1::Active(replay) => replay,
            GraphPublicationReplayLookupV1::Retired(_) => return Err(GraphDbError::Conflict),
            GraphPublicationReplayLookupV1::Missing => {
                return Err(GraphDbError::invalid(
                    "verified graph publication has no durable active replay record",
                ));
            }
        };
        let metadata_manifest = metadata_manifest_from_replay(&replay.publication, &check)?;
        let metadata_only = metadata_manifest.is_some();
        let has_supplied_manifest = supplied_manifest.is_some();
        let manifest = match supplied_manifest {
            Some(manifest) => {
                validate_supplied_manifest_binding(&replay.publication, &manifest, true, &check)?;
                manifest
            }
            None => match metadata_manifest {
                Some(manifest) => manifest,
                None => GraphGenerationManifest::from_replay(
                    &replay.publication,
                    self.inner.manifest_provider.as_ref(),
                    &check,
                )?,
            },
        };
        let apply_native = !metadata_only;
        let current = authority
            .verified_head(&publication_key.projection, context)
            .map_err(map_publication_error)?;
        if current != replay.publication.expected_prior_head {
            let historical_head = GraphVerifiedHeadV1::from_replay(
                &replay,
                replay.publication.expected_recovered_digest.clone(),
            )
            .map_err(|error| GraphDbError::Corrupt {
                message: format!("historical graph publication evidence is invalid: {error}"),
            })?;
            let is_current_head = current.as_ref() == Some(&historical_head);
            if is_current_head
                || current
                    .as_ref()
                    .is_some_and(|head| head.sequence > replay.sequence)
            {
                let mut visiting = BTreeSet::new();
                let dependencies = self.load_dependencies(
                    operation,
                    &database,
                    authority,
                    context,
                    &manifest,
                    &mut visiting,
                )?;
                // The journaled replay's expected recovered digest is
                // already proven to bind this exact manifest, so the
                // close/reopen recovered-digest proof verifies against it
                // directly instead of re-canonicalizing the full manifest.
                let sealed_digest = &replay.publication.expected_recovered_digest;
                let (historical_commit, recovered_digest) =
                    match (apply_native, has_supplied_manifest) {
                        (true, _) => {
                            let commit = database.apply_generation_unverified(&manifest, &check)?;
                            let (_, recovered) = database.reopen_and_verify_existing_generation(
                                &manifest,
                                sealed_digest,
                                &check,
                            )?;
                            (commit, recovered)
                        }
                        (false, true) => database.reopen_and_verify_existing_generation(
                            &manifest,
                            sealed_digest,
                            &check,
                        )?,
                        (false, false) if reopen_metadata => database
                            .reopen_and_verify_existing_generation(
                                &manifest,
                                sealed_digest,
                                &check,
                            )?,
                        (false, false) => {
                            database.verify_existing_generation(&manifest, sealed_digest, &check)?
                        }
                    };
                visiting.clear();
                self.load_verified_head(
                    operation,
                    &database,
                    authority,
                    context,
                    historical_head.clone(),
                    &mut visiting,
                )?;
                let lease = generation_lease(&manifest, historical_head.clone(), dependencies);
                if is_current_head {
                    // The durable CAS already advanced the head to this exact
                    // publication (an earlier publish crashed after its
                    // linearization point). Retrying it must seat the head for
                    // reads, not file its own publication as history and leave
                    // the projection without an installed verified head.
                    database.install_verified_generation(Arc::clone(&lease))?;
                } else {
                    database.remember_verified_generation(&lease)?;
                }
                let mut closure = BTreeMap::new();
                collect_closure(&lease, &mut closure)?;
                return Ok(VerifiedGraphCommit {
                    commit: historical_commit,
                    head: historical_head,
                    recovered_digest,
                    snapshot: VerifiedGraphSnapshot::new(database, lease, closure),
                });
            }
            return Err(GraphDbError::Conflict);
        }
        let mut visiting = BTreeSet::new();
        let dependencies = self.load_dependencies(
            operation,
            &database,
            authority,
            context,
            &manifest,
            &mut visiting,
        )?;

        // The journaled replay's expected recovered digest is already proven
        // to bind this exact manifest (inline decode, sealed identity pin, or
        // supplied-manifest binding above), so the close/reopen
        // recovered-digest proof verifies against it directly instead of
        // re-canonicalizing the full manifest a second time.
        let sealed_digest = &replay.publication.expected_recovered_digest;
        let verified = match (apply_native, has_supplied_manifest) {
            // A supplied manifest for a metadata-only replay carries the
            // native rows (vectors) the canonical source omits; a first
            // commit must install them natively before verification.
            (true, _) | (false, true) => {
                let commit = database.apply_generation_unverified(&manifest, &check)?;
                database
                    .reopen_and_verify_existing_generation(&manifest, sealed_digest, &check)
                    .map(|(_, recovered)| (commit, recovered))
            }
            (false, false) if reopen_metadata => {
                database.reopen_and_verify_existing_generation(&manifest, sealed_digest, &check)
            }
            (false, false) => database.verify_existing_generation(&manifest, sealed_digest, &check),
        };
        let (commit, recovered_digest) = match verified {
            Ok(verified) => verified,
            Err(error) => {
                if super::retains_fault(&error)
                    && let Err(retain_error) =
                        self.retain_verification_fault_for_lease(operation.database(), &error)
                {
                    return Err(crate::error::rollback_failure(
                        "retain graph generation verification fault",
                        error,
                        retain_error,
                    ));
                }
                return Err(error);
            }
        };
        operation.check(self, context)?;
        let cas = GraphVerifiedHeadCompareAndSwapV1 {
            publication_key: replay.publication.key.clone(),
            input_digest: replay.publication.input_digest.clone(),
            dependency_generation_closure_digest: replay
                .publication
                .dependency_generation_closure_digest
                .clone(),
            recovered_digest: recovered_digest.clone(),
            expected_prior_head: replay.publication.expected_prior_head.clone(),
        };
        let head = match authority
            .compare_and_swap_verified_head(&cas, context)
            .map_err(map_publication_error)?
        {
            GraphVerifiedHeadCasOutcomeV1::Advanced(head)
            | GraphVerifiedHeadCasOutcomeV1::ExactReplay(head) => head,
            GraphVerifiedHeadCasOutcomeV1::Conflict { .. }
            | GraphVerifiedHeadCasOutcomeV1::ReplayInputConflict { .. } => {
                return Err(GraphDbError::Conflict);
            }
            GraphVerifiedHeadCasOutcomeV1::RecoveredDigestMismatch { expected, actual } => {
                return Err(GraphDbError::GenerationMismatch {
                    namespace: manifest.projection.namespace.to_string(),
                    projection: manifest.projection.projection.to_string(),
                    generation: manifest.generation.to_string(),
                    message: format!(
                        "relational CAS expected recovered digest `{}`, observed `{}`",
                        expected.as_str(),
                        actual.as_str()
                    ),
                });
            }
            GraphVerifiedHeadCasOutcomeV1::MissingReplay => {
                return Err(GraphDbError::Corrupt {
                    message: "relational graph publication replay disappeared before CAS"
                        .to_owned(),
                });
            }
            GraphVerifiedHeadCasOutcomeV1::RetiredReplay(_) => {
                return Err(GraphDbError::Conflict);
            }
        };

        // Relational CAS is the linearization point. Caller cancellation is
        // deliberately not observed after it succeeds.
        let lease = generation_lease(&manifest, head.clone(), dependencies);
        database.install_verified_generation(Arc::clone(&lease))?;
        let mut closure = BTreeMap::new();
        collect_closure(&lease, &mut closure)?;
        Ok(VerifiedGraphCommit {
            commit,
            head,
            recovered_digest,
            snapshot: VerifiedGraphSnapshot::new(database, lease, closure),
        })
    }

    pub fn recover_verified_snapshot(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        projection: &GraphProjectionIdentityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        self.recover_verified_snapshot_with_operation(&operation, authority, context, projection)
    }

    /// Recovers through an already-issued, registry-validated graph lease.
    ///
    /// The exact mounted binding, locator, and live owner token are checked
    /// before recovery begins; foreign, absent, and retiring leases remain
    /// typed failures rather than reconstructed registrations.
    pub fn recover_verified_snapshot_with_lease(
        &self,
        database: &GraphDbLeaseV1,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        projection: &GraphProjectionIdentityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let operation = self.registered_operation_with_lease(database)?;
        self.recover_verified_snapshot_with_operation(&operation, authority, context, projection)
    }

    fn recover_verified_snapshot_with_operation(
        &self,
        operation: &RegisteredGraphDbOperationV1,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        projection: &GraphProjectionIdentityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        operation.check(self, context)?;
        operation.require_projection_binding(projection)?;
        let database = operation.database().clone();
        let head = authority
            .verified_head(projection, context)
            .map_err(map_publication_error)?
            .ok_or_else(|| {
                GraphDbError::unavailable("graph projection has no relational verified head")
            })?;
        let mut visiting = BTreeSet::new();
        let lease = self.load_verified_head(
            operation,
            &database,
            authority,
            context,
            head,
            &mut visiting,
        )?;
        database.install_verified_generation(Arc::clone(&lease))?;
        operation.check(self, context)?;
        let mut closure = BTreeMap::new();
        collect_closure(&lease, &mut closure)?;
        Ok(VerifiedGraphSnapshot::new(database, lease, closure))
    }

    pub fn verified_generation_snapshot(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        key: &GraphPublicationKeyV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        operation.check(self, context)?;
        operation.require_publication_binding(key)?;
        let database = operation.database().clone();
        let replay = authority
            .replay(key, context)
            .map_err(map_publication_error)?;
        let replay = match replay {
            GraphPublicationReplayLookupV1::Active(replay) => replay,
            GraphPublicationReplayLookupV1::Retired(_) => return Err(GraphDbError::Conflict),
            GraphPublicationReplayLookupV1::Missing => {
                return Err(GraphDbError::Corrupt {
                    message: "exact verified graph generation has no durable active replay"
                        .to_owned(),
                });
            }
        };
        let current = authority
            .verified_head(&key.projection, context)
            .map_err(map_publication_error)?
            .ok_or_else(|| {
                GraphDbError::unavailable("graph projection has no relational verified head")
            })?;
        if current.sequence < replay.sequence
            || (current.sequence == replay.sequence && current.key != replay.publication.key)
        {
            return Err(GraphDbError::Conflict);
        }
        let historical_head = GraphVerifiedHeadV1::from_replay(
            &replay,
            replay.publication.expected_recovered_digest.clone(),
        )
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("exact verified generation evidence is invalid: {error}"),
        })?;
        let mut visiting = BTreeSet::new();
        let lease = self.load_verified_head(
            &operation,
            &database,
            authority,
            context,
            historical_head,
            &mut visiting,
        )?;
        let mut closure = BTreeMap::new();
        collect_closure(&lease, &mut closure)?;
        operation.check(self, context)?;
        Ok(VerifiedGraphSnapshot::new(database, lease, closure))
    }

    fn load_dependencies(
        &self,
        operation: &RegisteredGraphDbOperationV1,
        database: &GraphDb,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        manifest: &GraphGenerationManifest,
        visiting: &mut BTreeSet<GenerationLocator>,
    ) -> Result<BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>, GraphDbError> {
        let mut loaded = BTreeMap::new();
        for dependency in &manifest.dependencies {
            let key = dependency_key_for_binding(operation.binding(), dependency)?;
            let replay = authority
                .replay(&key, context)
                .map_err(map_publication_error)?;
            let replay = require_active_replay_evidence(
                replay,
                &format!(
                    "dependency generation `{}/{}/{}` has no active relational replay",
                    dependency.projection.namespace,
                    dependency.projection.projection,
                    dependency.generation
                ),
            )?;
            let relational_head = authority
                .verified_head(&key.projection, context)
                .map_err(map_publication_error)?
                .ok_or(GraphDbError::Conflict)?;
            if relational_head.sequence < replay.sequence {
                return Err(GraphDbError::Conflict);
            }
            if relational_head.sequence == replay.sequence
                && relational_head.key != replay.publication.key
            {
                return Err(GraphDbError::Corrupt {
                    message: "dependency replay sequence aliases a different verified head"
                        .to_owned(),
                });
            }
            let head = GraphVerifiedHeadV1::from_replay(
                &replay,
                replay.publication.expected_recovered_digest.clone(),
            )
            .map_err(|error| GraphDbError::Corrupt {
                message: format!("dependency verified evidence is invalid: {error}"),
            })?;
            let lease =
                self.load_verified_head(operation, database, authority, context, head, visiting)?;
            loaded.insert(dependency.projection.clone(), lease);
        }
        validate_exact_dependency_closure(manifest, &loaded)?;
        Ok(loaded)
    }

    fn load_verified_head(
        &self,
        operation: &RegisteredGraphDbOperationV1,
        database: &GraphDb,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        head: GraphVerifiedHeadV1,
        visiting: &mut BTreeSet<GenerationLocator>,
    ) -> Result<Arc<VerifiedGenerationLease>, GraphDbError> {
        operation.check(self, context)?;
        let replay = authority
            .replay(&head.key, context)
            .map_err(map_publication_error)?;
        let replay = require_active_replay_evidence(
            replay,
            "verified graph head has no durable active replay",
        )?;
        require_head_replay(&head, &replay)?;
        let check = || operation.check(self, context);
        let metadata_manifest = metadata_manifest_from_replay(&replay.publication, &check)?;
        let manifest = match metadata_manifest {
            Some(manifest) => manifest,
            None => GraphGenerationManifest::from_replay(
                &replay.publication,
                self.inner.manifest_provider.as_ref(),
                &check,
            )?,
        };
        let locator =
            GenerationLocator::new(manifest.projection.clone(), manifest.generation.clone());
        if let Some(lease) = database.verified_generation(&locator)? {
            return Ok(lease);
        }
        if !visiting.insert(locator.clone()) {
            return Err(GraphDbError::Corrupt {
                message: "verified graph dependency closure contains a cycle".to_owned(),
            });
        }
        let dependencies =
            self.load_dependencies(operation, database, authority, context, &manifest, visiting)?;
        operation.check(self, context)?;
        let physical_namespace = locator.physical_namespace()?;
        match database
            .ensure_projection_readable(&physical_namespace, &manifest.projection.projection)
        {
            Ok(()) => {}
            Err(GraphDbError::ProjectionMismatch { message, .. }) => {
                return Err(GraphDbError::GenerationMismatch {
                    namespace: manifest.projection.namespace.to_string(),
                    projection: manifest.projection.projection.to_string(),
                    generation: manifest.generation.to_string(),
                    message,
                });
            }
            Err(error) => return Err(error),
        }
        let guard = database.read_guard()?;
        let native = guard.as_ref().ok_or(GraphDbError::Closed)?;
        // `require_head_replay` pinned this head to its journaled replay, and
        // the manifest was proven to bind that replay's digests when it was
        // decoded above, so the stored rows verify directly against the head
        // digest without canonicalizing the full manifest a second time.
        match verify_recovered_generation(native, &manifest, &head.recovered_digest, &check) {
            Ok(_) => {}
            Err(error @ GraphDbError::GenerationMismatch { .. }) => {
                drop(guard);
                database.quarantine_generation(&manifest)?;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
        drop(guard);
        let lease = generation_lease(&manifest, head, dependencies);
        database.remember_verified_generation(&lease)?;
        visiting.remove(&locator);
        Ok(lease)
    }
}
