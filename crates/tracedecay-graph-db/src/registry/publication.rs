use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_store::runtime::{
    GraphProjectionIdentityV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationProjectionPageRequestV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayPageRequestV1, GraphPublicationReplayRetirementV1,
    GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationStoreV1,
    GraphRecoveredGenerationDigestV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1,
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
use crate::state::latest_projection;
use crate::{
    GraphCommit, GraphDb, GraphDbError, GraphDbLeaseV1, GraphGenerationManifest,
    GraphGenerationManifestIdentity, GraphGenerationReplaySource, GraphProjectionIdentity,
    GraphReplayCollectionOutcome, VerifiedGraphCommit,
};

/// The three publication mode choices `publish_verified_inner` varies on.
///
/// Grouped because passing them positionally put the function at 8 arguments,
/// and three adjacent bools at a call site read as noise: `false, true` says
/// nothing about which knob is which.
struct GraphPublishModeV1 {
    /// A manifest supplied by the caller instead of one derived from replay.
    supplied_manifest: Option<Arc<GraphGenerationManifest>>,
    /// Reopen metadata rather than treating the existing handle as current.
    reopen_metadata: bool,
    /// This call writes a durable staging page, so it retries across the
    /// boundary to prove the exact commit rather than assuming it landed.
    durable_stage_boundary: bool,
}

impl GraphDbRegistry {
    #[hotpath::measure(label = "graph_db.replay_pool.retire", impl_type = "GraphDbRegistry")]
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
            let mut state = database.wait_verified_generations_write()?;
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

    #[hotpath::measure(label = "graph_db.replay_pool.finalize", impl_type = "GraphDbRegistry")]
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
        supplied_manifest: Option<Arc<GraphGenerationManifest>>,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        self.publish_verified_inner(
            &operation,
            authority,
            context,
            publication_key,
            GraphPublishModeV1 {
                supplied_manifest,
                reopen_metadata: false,
                durable_stage_boundary: false,
            },
        )
    }

    /// Publishes a native generation in two bounded, crash-safe attempts when
    /// this call writes any durable staging page. The retry proves the exact
    /// finalization receipt, then performs the mandatory close/reopen digest
    /// proof without repeating native staging.
    pub fn publish_verified_with_durable_stage_boundary(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
        supplied_manifest: Option<Arc<GraphGenerationManifest>>,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        self.publish_verified_inner(
            &operation,
            authority,
            context,
            publication_key,
            GraphPublishModeV1 {
                supplied_manifest,
                reopen_metadata: false,
                durable_stage_boundary: true,
            },
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
        self.publish_verified_inner(
            &operation,
            authority,
            context,
            publication_key,
            GraphPublishModeV1 {
                supplied_manifest: None,
                reopen_metadata: false,
                durable_stage_boundary: false,
            },
        )
    }

    pub(super) fn publish_ready_staged_generation(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        self.publish_verified_inner(
            &operation,
            authority,
            context,
            publication_key,
            GraphPublishModeV1 {
                supplied_manifest: None,
                reopen_metadata: true,
                durable_stage_boundary: false,
            },
        )
    }

    #[hotpath::measure(label = "graph_db.generation.publish", impl_type = "GraphDbRegistry")]
    fn publish_verified_inner(
        &self,
        operation: &RegisteredGraphDbOperationV1,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
        mode: GraphPublishModeV1,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let GraphPublishModeV1 {
            supplied_manifest,
            reopen_metadata,
            durable_stage_boundary,
        } = mode;
        operation.check(self, context)?;
        operation.require_publication_binding(publication_key)?;
        let database = operation.database().clone();
        database.record_memory_checkpoint(crate::hotpath_observe::GrafeoMemoryPhase::PublishStart);
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
                Some(manifest) => Arc::new(manifest),
                None => Arc::new(GraphGenerationManifest::from_replay(
                    &replay.publication,
                    self.inner.manifest_provider.as_ref(),
                    &check,
                )?),
            },
        };
        let apply_native = !metadata_only;
        database
            .record_memory_checkpoint(crate::hotpath_observe::GrafeoMemoryPhase::ReplayHydrated);
        // The identity and row counts are taken once, up front. Everything
        // after staging reads only these, so the bulk manifest can be handed
        // to staging and released there instead of living until this function
        // returns.
        let identity = manifest.identity();
        let (entity_rows, relation_rows) = manifest.row_counts();
        crate::hotpath_observe::record_counts(entity_rows, relation_rows, 1, 0);
        crate::hotpath_observe::record_hydration_source(if has_supplied_manifest {
            crate::hotpath_observe::HydrationSource::Supplied
        } else if metadata_only {
            crate::hotpath_observe::HydrationSource::Metadata
        } else {
            crate::hotpath_observe::HydrationSource::Replay
        });
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
                let locator = locator_from_key(&historical_head.key)?;
                // This exact mounted instance may already hold the verified
                // lease for this head: a racing publisher or an earlier
                // recover proved these stored rows against this same
                // recovered digest on this in-memory database moments ago.
                // Reuse is keyed on the exact instance, head, and recovered
                // digest (`lease.head == historical_head` compares all head
                // fields byte-exactly), the same trust decision the recover
                // fast path makes in `load_verified_head`. A fresh-from-disk
                // instance starts with an empty cache and pays the full
                // proof below.
                if let Some(lease) = database.verified_generation(&locator)?
                    && lease.head == historical_head
                {
                    operation.check(self, context)?;
                    let physical_namespace = locator.physical_namespace()?;
                    let commit = {
                        let guard = database.read_guard()?;
                        let native = guard.as_ref().ok_or(GraphDbError::Closed)?;
                        latest_projection(
                            native,
                            &physical_namespace,
                            &locator.projection.projection,
                        )?
                        .ok_or_else(|| GraphDbError::GenerationMismatch {
                            namespace: locator.projection.namespace.to_string(),
                            projection: locator.projection.projection.to_string(),
                            generation: locator.generation.to_string(),
                            message: "verified generation rows disappeared under a live lease"
                                .to_owned(),
                        })?
                        .commit
                    };
                    let recovered_digest = historical_head.recovered_digest.clone();
                    return seat_historical_verified_lease(
                        database,
                        lease,
                        historical_head,
                        is_current_head,
                        commit,
                        recovered_digest,
                    );
                }
                let mut visiting = BTreeSet::new();
                let dependencies = self.load_dependencies(
                    operation,
                    &database,
                    authority,
                    context,
                    &identity,
                    &mut visiting,
                )?;
                // The journaled replay's expected recovered digest is
                // already proven to bind this exact manifest, so the
                // close/reopen recovered-digest proof verifies against it
                // directly instead of re-canonicalizing the full manifest.
                let sealed_digest = &replay.publication.expected_recovered_digest;
                let row_counts = (entity_rows, relation_rows);
                let (historical_commit, recovered_digest) =
                    match (apply_native, has_supplied_manifest) {
                        (true, _) => {
                            // Staging consumes the manifest and drops its rows
                            // once the last page commit is durable, so the
                            // close/reopen below no longer overlaps them.
                            let staged = database
                                .apply_generation_unverified_with_digest_observed(
                                    manifest,
                                    sealed_digest,
                                    &check,
                                )?;
                            if durable_stage_boundary && staged.was_applied() {
                                return Err(GraphDbError::DeadlineExceeded);
                            }
                            let commit = staged.commit();
                            let (_, recovered) = database.reopen_and_verify_existing_generation(
                                &identity,
                                sealed_digest,
                                row_counts,
                                &check,
                            )?;
                            (commit, recovered)
                        }
                        (false, true) => {
                            drop(manifest);
                            database.reopen_and_verify_existing_generation(
                                &identity,
                                sealed_digest,
                                row_counts,
                                &check,
                            )?
                        }
                        (false, false) if reopen_metadata => {
                            drop(manifest);
                            database.reopen_and_verify_existing_generation(
                                &identity,
                                sealed_digest,
                                row_counts,
                                &check,
                            )?
                        }
                        (false, false) => {
                            drop(manifest);
                            database.verify_existing_generation(&identity, sealed_digest, &check)?
                        }
                    };
                // Seal implies an isolated compact store: adopt or build the
                // per-generation artifact from the rows the digest just
                // proved, before this head starts serving reads.
                database.ensure_sealed_generation_store(&identity, sealed_digest, &check)?;
                operation.check(self, context)?;
                // The digest proof above already streamed this generation's
                // stored rows against the head's journaled recovered digest
                // on this instance (`historical_head` carries exactly
                // `expected_recovered_digest`), and the dependency closure
                // was validated when it was loaded, so the verified lease is
                // built directly from that proof. Re-loading the head
                // through its replay would hydrate the manifest and stream
                // the rows a second time without adding durability.
                let physical_namespace = locator.physical_namespace()?;
                match database
                    .ensure_projection_readable(&physical_namespace, &identity.projection.projection)
                {
                    Ok(()) => {}
                    Err(GraphDbError::ProjectionMismatch { message, .. }) => {
                        return Err(GraphDbError::GenerationMismatch {
                            namespace: identity.projection.namespace.to_string(),
                            projection: identity.projection.projection.to_string(),
                            generation: identity.generation.to_string(),
                            message,
                        });
                    }
                    Err(error) => return Err(error),
                }
                let lease = generation_lease(&identity, historical_head.clone(), dependencies);
                return seat_historical_verified_lease(
                    database,
                    lease,
                    historical_head,
                    is_current_head,
                    historical_commit,
                    recovered_digest,
                );
            }
            return Err(GraphDbError::Conflict);
        }
        let mut visiting = BTreeSet::new();
        let dependencies = self.load_dependencies(
            operation,
            &database,
            authority,
            context,
            &identity,
            &mut visiting,
        )?;

        // The journaled replay's expected recovered digest is already proven
        // to bind this exact manifest (inline decode, sealed identity pin, or
        // supplied-manifest binding above), so the close/reopen
        // recovered-digest proof verifies against it directly instead of
        // re-canonicalizing the full manifest a second time.
        let sealed_digest = &replay.publication.expected_recovered_digest;
        let row_counts = (entity_rows, relation_rows);
        let verified = match (apply_native, has_supplied_manifest) {
            // A supplied manifest for a metadata-only replay carries the
            // native rows (vectors) the canonical source omits; a first
            // commit must install them natively before verification.
            //
            // Staging consumes the manifest and releases its bulk rows at the
            // last durable page commit, so the close/reopen and the streamed
            // recovered-digest proof below run without them resident.
            (true, _) | (false, true) => {
                let staged = database.apply_generation_unverified_with_digest_observed(
                    manifest,
                    sealed_digest,
                    &check,
                )?;
                if durable_stage_boundary && staged.was_applied() {
                    return Err(GraphDbError::DeadlineExceeded);
                }
                let commit = staged.commit();
                database
                    .reopen_and_verify_existing_generation(
                        &identity,
                        sealed_digest,
                        row_counts,
                        &check,
                    )
                    .map(|(_, recovered)| (commit, recovered))
            }
            (false, false) if reopen_metadata => {
                drop(manifest);
                database.reopen_and_verify_existing_generation(
                    &identity,
                    sealed_digest,
                    row_counts,
                    &check,
                )
            }
            (false, false) => {
                drop(manifest);
                database.verify_existing_generation(&identity, sealed_digest, &check)
            }
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
        database
            .record_memory_checkpoint(crate::hotpath_observe::GrafeoMemoryPhase::NativeVerified);
        // Seal implies an isolated compact store. Built after the recovered
        // digest proved the staged rows and before the relational CAS, so a
        // build failure is a typed, retryable publication error rather than a
        // post-linearization surprise. The artifact is digest-bound to this
        // exact generation, so a competing publisher adopting it is safe.
        database.ensure_sealed_generation_store(&identity, sealed_digest, &check)?;
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
                    namespace: identity.projection.namespace.to_string(),
                    projection: identity.projection.projection.to_string(),
                    generation: identity.generation.to_string(),
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
        let lease = generation_lease(&identity, head.clone(), dependencies);
        database.install_verified_generation(Arc::clone(&lease))?;
        database.record_memory_checkpoint(crate::hotpath_observe::GrafeoMemoryPhase::Published);
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

    #[hotpath::measure(label = "graph_db.generation.recover", impl_type = "GraphDbRegistry")]
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
        database.record_memory_checkpoint(crate::hotpath_observe::GrafeoMemoryPhase::RecoveryStart);
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
        database.record_memory_checkpoint(crate::hotpath_observe::GrafeoMemoryPhase::Recovered);
        operation.check(self, context)?;
        let mut closure = BTreeMap::new();
        collect_closure(&lease, &mut closure)?;
        Ok(VerifiedGraphSnapshot::new(database, lease, closure))
    }

    #[hotpath::measure(
        label = "graph_db.generation.recover.historical",
        impl_type = "GraphDbRegistry"
    )]
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
        identity: &GraphGenerationManifestIdentity,
        visiting: &mut BTreeSet<GenerationLocator>,
    ) -> Result<BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>, GraphDbError> {
        let mut loaded = BTreeMap::new();
        for dependency in &identity.dependencies {
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
        validate_exact_dependency_closure(identity, &loaded)?;
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
        let locator = locator_from_key(&head.key)?;
        if let Some(lease) = database.verified_generation(&locator)?
            && lease.head == head
        {
            return Ok(lease);
        }
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
        // Only the identity is needed from here on: the closure walk, the
        // readability check, and the recovered-digest proof all read metadata
        // or stream stored rows. Releasing the decoded manifest here keeps a
        // dependency hydration from holding a second full row set alive while
        // the proof runs.
        let identity = manifest.identity();
        drop(manifest);
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        if let Some(lease) = database.verified_generation(&locator)? {
            return Ok(lease);
        }
        if !visiting.insert(locator.clone()) {
            return Err(GraphDbError::Corrupt {
                message: "verified graph dependency closure contains a cycle".to_owned(),
            });
        }
        let dependencies =
            self.load_dependencies(operation, database, authority, context, &identity, visiting)?;
        operation.check(self, context)?;
        let physical_namespace = locator.physical_namespace()?;
        match database
            .ensure_projection_readable(&physical_namespace, &identity.projection.projection)
        {
            Ok(()) => {}
            Err(GraphDbError::ProjectionMismatch { message, .. }) => {
                return Err(GraphDbError::GenerationMismatch {
                    namespace: identity.projection.namespace.to_string(),
                    projection: identity.projection.projection.to_string(),
                    generation: identity.generation.to_string(),
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
        match verify_recovered_generation(native, &identity, &head.recovered_digest, &check) {
            Ok(_) => {}
            Err(error @ GraphDbError::GenerationMismatch { .. }) => {
                drop(guard);
                database.quarantine_generation(&identity)?;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
        drop(guard);
        // Recovery adopts a matching sealed compact artifact from disk when
        // one exists; anything stale or unreadable is discarded and reads
        // stay on the staging rows just verified above.
        database.open_sealed_generation_store_if_present(&identity, &head.recovered_digest)?;
        let lease = generation_lease(&identity, head, dependencies);
        database.remember_verified_generation(&lease)?;
        visiting.remove(&locator);
        Ok(lease)
    }
}

/// Seats a verified lease for a historical (already durably linearized)
/// publication and assembles its commit receipt.
///
/// The lease is either freshly built from a digest proof this call just ran,
/// or reused from this exact instance's verified-generation cache; both carry
/// the same instance-bound proof, so seating is identical.
fn seat_historical_verified_lease(
    database: GraphDbLeaseV1,
    lease: Arc<VerifiedGenerationLease>,
    head: GraphVerifiedHeadV1,
    is_current_head: bool,
    commit: GraphCommit,
    recovered_digest: GraphRecoveredGenerationDigestV1,
) -> Result<VerifiedGraphCommit, GraphDbError> {
    if is_current_head {
        // The durable CAS already advanced the head to this exact
        // publication (an earlier publish crashed after its linearization
        // point, or a racing publisher won). Retrying it must seat the head
        // for reads, not file its own publication as history and leave the
        // projection without an installed verified head.
        database.install_verified_generation(Arc::clone(&lease))?;
    } else {
        database.remember_verified_generation(&lease)?;
    }
    database.record_memory_checkpoint(crate::hotpath_observe::GrafeoMemoryPhase::Published);
    let mut closure = BTreeMap::new();
    collect_closure(&lease, &mut closure)?;
    Ok(VerifiedGraphCommit {
        commit,
        head,
        recovered_digest,
        snapshot: VerifiedGraphSnapshot::new(database, lease, closure),
    })
}

/// Unit tests live here (not in `tests/`) because they assert on the
/// crate-private, `cfg(test)`-only `RECOVERED_GENERATION_ENUMERATIONS`
/// counter, which counts full stored-row digest proofs.
#[cfg(test)]
mod historical_publication_reuse_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use tempfile::TempDir;
    use tracedecay_domain::UtcMicros;
    use tracedecay_store::{
        BrainId, GraphPublicationInputDigestV1, GraphPublicationKeyV1,
        GraphPublicationOperationContextV1, GraphPublicationProjectionPageRequestV1,
        GraphPublicationProjectionPageV1, GraphPublicationReplayLookupV1,
        GraphPublicationReplayPageRequestV1, GraphPublicationReplayPageV1,
        GraphPublicationReplayRecordV1, GraphPublicationReplayRetirementV1,
        GraphPublicationReplayV1, GraphPublicationRetiredCleanupPageRequestV1,
        GraphPublicationRetiredCleanupPageV1, GraphPublicationSequenceV1,
        GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1, GraphPublicationStoreV1,
        GraphProjectionIdentityV1, GraphReplayAppendOutcomeV1,
        GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
        GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1, ProjectId,
        RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
        RetainedGraphStoreOwnerOperationLeaseErrorV1, RuntimeCancellationIdV1,
        RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
        RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
        StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
        UserProfileId, VerifiedStoreLocatorV1, canonical_store_locator_digest,
    };
    use tracedecay_store::runtime::GraphReplayRetirementOutcomeV1;

    use crate::generation::{
        recovered_generation_enumerations, reset_recovered_generation_enumerations,
    };
    use crate::{
        GraphCancellation, GraphDbOwnerRegistrationV1, GraphDbRegistration, GraphDbRegistry,
        GraphDbRegistryConfig, GraphEntity, GraphEntityId, GraphGenerationId,
        GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace, GraphProjectionId,
        GraphProjectionIdentity, GraphProperty, GraphPropertyName, GraphWatermark,
        SourceGeneration,
    };

    #[derive(Debug)]
    struct TestCancellation;

    impl GraphCancellation for TestCancellation {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    #[derive(Debug)]
    struct TestGraphLease {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        canonical_path: PathBuf,
    }

    impl RetainedGraphStoreLeaseV1 for TestGraphLease {
        fn binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.verified_locator
        }

        fn canonical_path(&self) -> &Path {
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

        fn canonical_path(&self) -> &Path {
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

    struct TestProbe {
        cancellation: RuntimeCancellationIdentityV1,
        deadline: RuntimeDeadlineV1,
        commit_started: AtomicBool,
    }

    impl RuntimeRequestProbeV1 for TestProbe {
        fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
            &self.cancellation
        }

        fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
            &self.deadline
        }

        fn interruption(&self) -> Option<RuntimeInterruptionV1> {
            None
        }

        fn try_begin_commit(&self) -> bool {
            self.commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        }
    }

    fn control_and_probe() -> (RuntimeRequestControlV1, TestProbe) {
        let cancellation = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new("reuse-test-cancellation").unwrap(),
            generation: 1,
        };
        let deadline = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new("reuse-test-deadline").unwrap(),
        };
        (
            RuntimeRequestControlV1 {
                requested_at: UtcMicros(1),
                deadline: deadline.clone(),
                cancellation: cancellation.clone(),
            },
            TestProbe {
                cancellation,
                deadline,
                commit_started: AtomicBool::new(false),
            },
        )
    }

    /// Journal-and-head fake for the publish/recover flows under test. The
    /// replay-pool collection surfaces (pages, retirement, cleanup) answer a
    /// typed infrastructure failure so any unexpected reach into them fails
    /// the test loudly instead of succeeding vacuously.
    #[derive(Default)]
    struct RecordedAuthority {
        next_sequence: u64,
        records: BTreeMap<GraphPublicationKeyV1, GraphPublicationReplayRecordV1>,
        pending: BTreeMap<GraphProjectionIdentityV1, GraphPublicationReplayRecordV1>,
        heads: BTreeMap<GraphProjectionIdentityV1, GraphVerifiedHeadV1>,
    }

    impl RecordedAuthority {
        fn stage(
            &mut self,
            publication: GraphPublicationReplayV1,
        ) -> GraphPublicationReplayRecordV1 {
            self.next_sequence += 1;
            let record = GraphPublicationReplayRecordV1::new(
                GraphPublicationSequenceV1::new(self.next_sequence).unwrap(),
                publication,
            )
            .unwrap();
            self.records
                .insert(record.publication.key.clone(), record.clone());
            self.pending
                .insert(record.publication.key.projection.clone(), record.clone());
            record
        }
    }

    impl GraphPublicationStoreV1 for RecordedAuthority {
        fn append_replay(
            &mut self,
            publication: &GraphPublicationReplayV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphReplayAppendOutcomeV1> {
            if let Some(record) = self.records.get(&publication.key) {
                return Ok(GraphReplayAppendOutcomeV1::ExactReplay(record.clone()));
            }
            Ok(GraphReplayAppendOutcomeV1::Appended(
                self.stage(publication.clone()),
            ))
        }

        fn pending_replay(
            &mut self,
            projection: &GraphProjectionIdentityV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
            Ok(self.pending.get(projection).cloned())
        }

        fn replay(
            &mut self,
            key: &GraphPublicationKeyV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphPublicationReplayLookupV1> {
            Ok(match self.records.get(key) {
                Some(record) => GraphPublicationReplayLookupV1::Active(record.clone()),
                None => GraphPublicationReplayLookupV1::Missing,
            })
        }

        fn replay_page(
            &mut self,
            _request: &GraphPublicationReplayPageRequestV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphPublicationReplayPageV1> {
            Err(GraphPublicationStoreErrorV1::Infrastructure)
        }

        fn projection_page(
            &mut self,
            _request: &GraphPublicationProjectionPageRequestV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphPublicationProjectionPageV1> {
            Err(GraphPublicationStoreErrorV1::Infrastructure)
        }

        fn retire_replay(
            &mut self,
            _request: &GraphPublicationReplayRetirementV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1> {
            Err(GraphPublicationStoreErrorV1::Infrastructure)
        }

        fn retired_cleanup_page(
            &mut self,
            _request: &GraphPublicationRetiredCleanupPageRequestV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphPublicationRetiredCleanupPageV1> {
            Err(GraphPublicationStoreErrorV1::Infrastructure)
        }

        fn finalize_retired_replay_cleanup(
            &mut self,
            _request: &GraphPublicationReplayRetirementV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphRetiredReplayCleanupFinalizeOutcomeV1> {
            Err(GraphPublicationStoreErrorV1::Infrastructure)
        }

        fn verified_head(
            &mut self,
            projection: &GraphProjectionIdentityV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>> {
            Ok(self.heads.get(projection).cloned())
        }

        fn compare_and_swap_verified_head(
            &mut self,
            request: &GraphVerifiedHeadCompareAndSwapV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphVerifiedHeadCasOutcomeV1> {
            let record = self
                .records
                .get(&request.publication_key)
                .cloned()
                .ok_or(GraphPublicationStoreErrorV1::Infrastructure)?;
            if self.heads.get(&request.publication_key.projection)
                != request.expected_prior_head.as_ref()
            {
                return Ok(GraphVerifiedHeadCasOutcomeV1::Conflict {
                    actual: self.heads.get(&request.publication_key.projection).cloned(),
                });
            }
            let head = GraphVerifiedHeadV1::from_replay(&record, request.recovered_digest.clone())
                .unwrap();
            self.heads
                .insert(request.publication_key.projection.clone(), head.clone());
            self.pending.remove(&request.publication_key.projection);
            Ok(GraphVerifiedHeadCasOutcomeV1::Advanced(head))
        }
    }

    fn binding() -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                BrainId::try_from("brain.publication-reuse".to_owned()).unwrap(),
                UserProfileId::try_from("profile.publication-reuse".to_owned()).unwrap(),
                ProjectId::try_from("project.publication-reuse".to_owned()).unwrap(),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        )
    }

    fn registration(binding: StoreRuntimeBindingV1, root: &Path) -> GraphDbRegistration {
        let canonical_path = root.join("graph.grafeo");
        let verified_locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            canonical_store_locator_digest(&canonical_path).unwrap(),
        );
        GraphDbRegistration {
            authority_lease: Arc::new(TestGraphLease {
                binding,
                verified_locator,
                canonical_path,
            }),
            cancellation: Arc::new(TestCancellation),
            lifecycle_cancellation: Arc::new(TestCancellation),
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    fn mount(registry: &GraphDbRegistry, binding: &StoreRuntimeBindingV1, root: &Path) {
        let operation = registration(binding.clone(), root);
        let authority_attachment = Box::new(TestGraphLease {
            binding: operation.authority_lease.binding().clone(),
            verified_locator: operation.authority_lease.verified_locator().clone(),
            canonical_path: operation.authority_lease.canonical_path().to_path_buf(),
        });
        let attachment = registry
            .resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
                operation,
                authority_attachment,
            })
            .unwrap();
        drop(attachment);
    }

    fn test_manifest(projection: GraphProjectionIdentity) -> GraphGenerationManifest {
        GraphGenerationManifest::new(
            projection,
            GraphGenerationId::new("reuse-g1").unwrap(),
            SourceGeneration::new("source:reuse-g1".to_owned()).unwrap(),
            GraphWatermark::new("watermark:reuse-g1".to_owned()).unwrap(),
            Vec::new(),
            vec![
                GraphEntity::new(
                    GraphEntityId::new("entity:reuse").unwrap(),
                    BTreeSet::new(),
                    BTreeMap::from([(
                        GraphPropertyName::new("marker").unwrap(),
                        GraphProperty::String("reuse".to_owned()),
                    )]),
                )
                .unwrap(),
            ],
            Vec::new(),
        )
        .unwrap()
    }

    struct PublishedFixture {
        _temp: TempDir,
        registry: GraphDbRegistry,
        binding: StoreRuntimeBindingV1,
        root: PathBuf,
        authority: RecordedAuthority,
        key: GraphPublicationKeyV1,
        head: GraphVerifiedHeadV1,
        generation: GraphGenerationId,
        projection: GraphProjectionIdentity,
    }

    /// Publishes one inline-manifest generation and asserts its proof
    /// streamed the stored rows exactly once, so every later assertion on
    /// the enumeration counter is against a live, observed baseline.
    fn published_fixture() -> PublishedFixture {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let binding = binding();
        mount(&registry, &binding, &root);
        let mut authority = RecordedAuthority::default();
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new("namespace:publication-reuse").unwrap(),
            GraphProjectionId::new("code").unwrap(),
        );
        let manifest = test_manifest(projection.clone());
        let record = authority.stage(
            manifest
                .relational_replay(
                    binding.shard_id.clone(),
                    GraphIdempotencyKey::new("publish:reuse-g1").unwrap(),
                    GraphPublicationInputDigestV1::new(format!("sha256:{}", "a".repeat(64)))
                        .unwrap(),
                    None,
                    &|| Ok(()),
                )
                .unwrap(),
        );
        let key = record.publication.key.clone();
        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        reset_recovered_generation_enumerations();
        let first = registry
            .publish_verified(
                registration(binding.clone(), &root),
                &mut authority,
                &context,
                &key,
                None,
            )
            .unwrap();
        assert_eq!(
            recovered_generation_enumerations(),
            1,
            "first publication must stream the recovered-digest proof exactly once"
        );
        let head = first.head.clone();
        assert_eq!(first.snapshot.generation(), &manifest.generation);
        let generation = manifest.generation.clone();
        drop(first);
        PublishedFixture {
            _temp: temp,
            registry,
            binding,
            root,
            authority,
            key,
            head,
            generation,
            projection,
        }
    }

    /// The recover-after-publish idempotent arm: republishing the exact
    /// journaled key whose verified head is already current must reuse the
    /// lease this same mounted instance proved moments earlier — zero
    /// additional stored-row enumerations — and still seat the head for
    /// reads. A follow-up recover on the same instance stays cache-served.
    #[test]
    fn recover_after_publish_reuses_the_instance_proof() {
        let mut fixture = published_fixture();
        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        reset_recovered_generation_enumerations();
        let republished = fixture
            .registry
            .publish_verified(
                registration(fixture.binding.clone(), &fixture.root),
                &mut fixture.authority,
                &context,
                &fixture.key,
                None,
            )
            .unwrap();
        assert_eq!(republished.head, fixture.head);
        assert_eq!(republished.recovered_digest, fixture.head.recovered_digest);
        assert_eq!(republished.snapshot.generation(), &fixture.generation);
        assert_eq!(
            recovered_generation_enumerations(),
            0,
            "an idempotent republication on the proving instance must not re-enumerate the stored rows"
        );
        let seated = fixture
            .registry
            .verified_snapshot(
                registration(fixture.binding.clone(), &fixture.root),
                &fixture.projection,
            )
            .unwrap();
        assert_eq!(seated.generation(), &fixture.generation);
        drop(republished);
        drop(seated);

        reset_recovered_generation_enumerations();
        let recovered = fixture
            .registry
            .recover_verified_snapshot(
                registration(fixture.binding.clone(), &fixture.root),
                &mut fixture.authority,
                &context,
                &fixture.key.projection,
            )
            .unwrap();
        assert_eq!(recovered.generation(), &fixture.generation);
        assert_eq!(
            recovered_generation_enumerations(),
            0,
            "a recover on the proving instance must stay cache-served"
        );
    }

    /// A crash-recovery republication on a genuinely fresh-from-disk
    /// instance must pay the full recovered-digest proof — but exactly once.
    /// Before the duplicate-proof fix this path enumerated the stored rows
    /// twice: once for the close/reopen digest proof and once more re-loading
    /// the head it had just proven.
    #[test]
    fn fresh_instance_republication_streams_the_proof_exactly_once() {
        let mut fixture = published_fixture();
        assert!(
            fixture
                .registry
                .close(&registration(fixture.binding.clone(), &fixture.root))
                .unwrap()
        );
        mount(&fixture.registry, &fixture.binding, &fixture.root);
        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        reset_recovered_generation_enumerations();
        let resumed = fixture
            .registry
            .publish_verified(
                registration(fixture.binding.clone(), &fixture.root),
                &mut fixture.authority,
                &context,
                &fixture.key,
                None,
            )
            .unwrap();
        assert_eq!(resumed.head, fixture.head);
        assert_eq!(resumed.snapshot.generation(), &fixture.generation);
        assert_eq!(
            recovered_generation_enumerations(),
            1,
            "a fresh-from-disk republication must stream the full proof exactly once, not twice"
        );
    }

    /// The reuse never leaks across instances: a recover on a fresh
    /// re-mounted instance of the same store must run the full
    /// recovered-digest proof.
    #[test]
    fn a_fresh_from_disk_open_still_pays_the_recovered_digest_proof() {
        let mut fixture = published_fixture();
        assert!(
            fixture
                .registry
                .close(&registration(fixture.binding.clone(), &fixture.root))
                .unwrap()
        );
        mount(&fixture.registry, &fixture.binding, &fixture.root);
        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        reset_recovered_generation_enumerations();
        let recovered = fixture
            .registry
            .recover_verified_snapshot(
                registration(fixture.binding.clone(), &fixture.root),
                &mut fixture.authority,
                &context,
                &fixture.key.projection,
            )
            .unwrap();
        assert_eq!(recovered.generation(), &fixture.generation);
        assert_eq!(recovered.verified_head(), &fixture.head);
        assert_eq!(
            recovered_generation_enumerations(),
            1,
            "a genuinely fresh-from-disk open must pay the full recovered-digest proof"
        );
    }
}
