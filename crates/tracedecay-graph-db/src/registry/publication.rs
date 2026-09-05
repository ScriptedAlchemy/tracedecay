use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_store::runtime::{
    GraphPendingReplayDiscardOutcomeV1, GraphPendingReplayDiscardV1, GraphProjectionIdentityV1,
    GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationProjectionPageRequestV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayPageRequestV1, GraphPublicationReplayRecordV1,
    GraphPublicationReplayRetirementV1, GraphPublicationRetiredCleanupPageRequestV1,
    GraphPublicationStoreV1, GraphRecoveredGenerationDigestV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1,
    MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1, MAX_GRAPH_REPLAY_PAGE_RECORDS_V1,
};

use super::code_graph_namespace::is_legacy_per_generation_code_graph_namespace_str;
use super::path::canonical_graph_database_file;
use super::publication_support::{
    RegisteredGraphDbOperationV1, check_all, clear_retiring_fence, collect_closure,
    dependency_key_for_binding, locator_from_dependency, locator_from_key, map_publication_error,
    require_active_replay_evidence, require_head_replay, require_projection_binding,
    retain_lease_closure, validate_exact_dependency_closure, validate_replay_cursor,
};
use super::{GraphDbRegistration, GraphDbRegistry, check_registration_request};
use crate::generation::{metadata_manifest_from_replay, validate_supplied_manifest_binding};
use crate::generation_runtime::{GenerationContentsDeletion, GenerationStageOutcome};
use crate::lease::{
    GenerationLocator, VerifiedGenerationLease, VerifiedGraphSnapshot, generation_lease,
};
use crate::{
    GraphCommit, GraphDb, GraphDbError, GraphDbLeaseV1, GraphGenerationManifest,
    GraphGenerationManifestIdentity, GraphGenerationReplaySource, GraphProjectionIdentity,
    GraphReplayCollectionOutcome, SealedStagingRelease, SealedStagingRetentionReason,
    VerifiedGraphCommit,
};

/// The publication mode choices `publish_verified_inner` varies on.
///
/// Grouped because passing them positionally put the function at 8 arguments,
/// and adjacent bools at a call site read as noise: `false, true` says
/// nothing about which knob is which.
struct GraphPublishModeV1 {
    /// A manifest supplied by the caller instead of one derived from replay.
    supplied_manifest: Option<Arc<GraphGenerationManifest>>,
    /// Reopen metadata rather than treating the existing handle as current.
    reopen_metadata: bool,
}

/// Exact persisted identity emitted by the shipped per-generation code-graph
/// layout. This predicate gates destructive cleanup, so the broader reporting
/// classifier is deliberately insufficient here.
fn is_shipped_legacy_code_graph_projection(projection: &GraphProjectionIdentityV1) -> bool {
    let Some(digest) = projection
        .namespace
        .as_str()
        .strip_prefix(crate::LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX)
    else {
        return false;
    };
    projection.projection.as_str() == "code-graph"
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod legacy_cleanup_identity_tests {
    use super::is_shipped_legacy_code_graph_projection;
    use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
    use tracedecay_store::{
        GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1, StoreShardIdV1,
    };

    fn projection(namespace: String, projection: &str) -> GraphProjectionIdentityV1 {
        GraphProjectionIdentityV1 {
            shard_id: StoreShardIdV1::project(
                BrainId::new("brain.legacy-cleanup").unwrap(),
                UserProfileId::new("profile.legacy-cleanup").unwrap(),
                ProjectId::new("project.legacy-cleanup").unwrap(),
            ),
            namespace: GraphNamespaceV1::new(namespace).unwrap(),
            projection: GraphProjectionIdV1::new(projection).unwrap(),
        }
    }

    #[test]
    fn destructive_legacy_cleanup_requires_the_exact_shipped_projection_identity() {
        let prefix = crate::LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX;
        assert!(is_shipped_legacy_code_graph_projection(&projection(
            format!("{prefix}{}", "1a".repeat(32)),
            "code-graph",
        )));
        assert!(!is_shipped_legacy_code_graph_projection(&projection(
            format!("{prefix}{}", "a".repeat(63)),
            "code-graph",
        )));
        assert!(!is_shipped_legacy_code_graph_projection(&projection(
            format!("{prefix}{}", "A".repeat(64)),
            "code-graph",
        )));
        assert!(!is_shipped_legacy_code_graph_projection(&projection(
            format!("{prefix}{}", "b".repeat(64)),
            "semantic-vector",
        )));
    }
}

/// A publication whose durable generation proof completed but whose
/// relational verified-head CAS has not yet run.
///
/// This is the boundary that lets a serving gate cover only the atomic swap:
/// everything in here — native staging, the sealed-store build, and the
/// recovered-digest proof — reads the immutable staged generation and writes
/// derived artifacts, so it runs without any publication gate held. The CAS
/// and the read-side lease install in
/// [`GraphDbRegistry::complete_verified_publication`] are the only phases a
/// caller needs to serialize against readers of the freshly published head.
///
/// The retained lease pins the exact mounted database instance the proof ran
/// on; completion re-validates that the lease still names the current
/// registry owner, so a store that was retired and remounted between the two
/// phases answers a typed conflict instead of installing an instance-foreign
/// proof.
pub struct ProvenGraphPublicationV1 {
    database: GraphDbLeaseV1,
    identity: GraphGenerationManifestIdentity,
    dependencies: BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
    commit: GraphCommit,
    recovered_digest: GraphRecoveredGenerationDigestV1,
    replay: GraphPublicationReplayRecordV1,
}

/// Outcome of [`GraphDbRegistry::prepare_verified_publication`].
pub enum GraphPublicationPreparationV1 {
    /// The publication was already durably linearized (an idempotent replay
    /// or a historical head); the snapshot is ready and no CAS is pending.
    Settled(Box<VerifiedGraphCommit>),
    /// The durable proof completed; the relational CAS and read-side install
    /// remain, through [`GraphDbRegistry::complete_verified_publication`].
    Proven(Box<ProvenGraphPublicationV1>),
}

impl GraphDbRegistry {
    /// Recovers a dependency-free verified code graph directly from its
    /// sealed derived store and the exact relational head/replay authority.
    ///
    /// This path never resolves or registers the shared mutable staging
    /// database. Any absent, foreign, corrupt, dependency-bearing, or
    /// non-code replay fails closed so the caller can take the ordinary
    /// staging recovery path explicitly.
    #[hotpath::measure(
        label = "graph_db.generation.recover.direct_sealed",
        impl_type = "GraphDbRegistry"
    )]
    pub fn recover_verified_sealed_snapshot(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        projection: &GraphProjectionIdentityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        check_all(&registration, context, "generation.recover.direct_sealed")?;
        require_projection_binding(&registration, projection)?;
        let head = authority
            .verified_head(projection, context)
            .map_err(map_publication_error)?
            .ok_or_else(|| {
                GraphDbError::unavailable("graph projection has no relational verified head")
            })?;
        let replay = authority
            .replay(&head.key, context)
            .map_err(map_publication_error)?;
        let replay = require_active_replay_evidence(
            replay,
            "verified graph head has no durable active replay",
        )?;
        require_head_replay(&head, &replay)?;
        if !replay.publication.direct_dependency_generations.is_empty() {
            return Err(GraphDbError::unavailable(
                "dependency-bearing graph generation requires staging recovery",
            ));
        }
        let source = crate::generation::checked_decode_replay_source(
            &replay.publication.canonical_replay_source,
            &|| check_all(&registration, context, "generation.recover.direct_sealed"),
        )?;
        let GraphGenerationReplaySource::SealedCodeGeneration(source) = source else {
            return Err(GraphDbError::unavailable(
                "graph replay has no sealed code-generation source",
            ));
        };
        source
            .repository
            .validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        source
            .generation
            .validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;

        let locator = locator_from_key(&head.key)?;
        let database_path = canonical_graph_database_file(registration.canonical_path())?;
        let (database, identity) = crate::sealed_store::open_direct_sealed_generation(
            &database_path,
            locator.projection,
            locator.generation,
            &head.recovered_digest,
            Arc::clone(&registration.authority_lease),
            &|| check_all(&registration, context, "generation.recover.direct_sealed"),
        )?
        .ok_or_else(|| GraphDbError::unavailable("sealed generation store is absent"))?;
        if identity.dependency_closure_digest(&|| {
            check_all(&registration, context, "generation.recover.direct_sealed")
        })? != head.dependency_generation_closure_digest
        {
            return Err(GraphDbError::Corrupt {
                message: "sealed generation dependency closure does not match its verified head"
                    .to_owned(),
            });
        }
        check_all(&registration, context, "generation.recover.direct_sealed")?;
        let lease = generation_lease(&identity, head, BTreeMap::new());
        self.track_direct_sealed_reader(&lease)?;
        Ok(VerifiedGraphSnapshot::new_direct_sealed(database, lease))
    }

    pub fn release_sealed_generation_staging_rows(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        projection: &GraphProjectionIdentityV1,
    ) -> Result<SealedStagingRelease, GraphDbError> {
        check_all(&registration, context, "generation.release_sealed_staging")?;
        require_projection_binding(&registration, projection)?;
        let database = self.resolve(registration.clone())?;
        // This bounded maintenance entrypoint is also the retry clock for an
        // installed sealed reader whose prior non-blocking hibernation pass
        // met an active operation. Identity and receipt stay installed; only
        // idle native engines are released.
        database.reap_idle_sealed_generation_engines(None);
        let relational_head = authority
            .verified_head(projection, context)
            .map_err(map_publication_error)?;
        let (key, direct_dependencies, canonical_replay_source, relational_recovered_digest) =
            if let Some(head) = &relational_head {
                let replay = authority
                    .replay(&head.key, context)
                    .map_err(map_publication_error)?;
                let replay = require_active_replay_evidence(
                    replay,
                    "verified graph head has no durable active replay",
                )?;
                require_head_replay(head, &replay)?;
                (
                    replay.publication.key,
                    replay.publication.direct_dependency_generations,
                    replay.publication.canonical_replay_source,
                    head.recovered_digest.clone(),
                )
            } else {
                if !is_shipped_legacy_code_graph_projection(projection)
                    || authority
                        .pending_replay(projection, context)
                        .map_err(map_publication_error)?
                        .is_some()
                {
                    return Ok(SealedStagingRelease::Retained(
                        SealedStagingRetentionReason::NoVerifiedLease,
                    ));
                }
                // The shipped pre-cutover layout put one code generation in one
                // projection. Its head and active replay are retired atomically;
                // the retained cleanup tombstone is the durable proof that the
                // publication completed and that only derived native bytes remain.
                // An active replay without a head is always pending in the
                // production authority and can never authorize deletion.
                let mut selected = None;
                let mut after = None;
                loop {
                    let request = GraphPublicationRetiredCleanupPageRequestV1::new(
                        projection.clone(),
                        after.clone(),
                        MAX_GRAPH_REPLAY_PAGE_RECORDS_V1,
                    )
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
                    let page = authority
                        .retired_cleanup_page(&request, context)
                        .map_err(map_publication_error)?;
                    for tombstone in page.records {
                        if tombstone.key.projection != *projection {
                            return Err(GraphDbError::Corrupt {
                                message: "legacy graph cleanup page escaped its projection"
                                    .to_owned(),
                            });
                        }
                        if selected.replace(tombstone).is_some() {
                            return Ok(SealedStagingRelease::Retained(
                                SealedStagingRetentionReason::NoVerifiedLease,
                            ));
                        }
                    }
                    let Some(continuation) = page.continuation else {
                        break;
                    };
                    validate_replay_cursor(
                        projection,
                        after.as_ref(),
                        &continuation,
                        "legacy graph cleanup staging release",
                    )?;
                    after = Some(continuation);
                }
                let Some(tombstone) = selected else {
                    return Ok(SealedStagingRelease::Retained(
                        SealedStagingRetentionReason::NoVerifiedLease,
                    ));
                };
                let source =
                    tombstone
                        .canonical_replay_source
                        .ok_or_else(|| GraphDbError::Corrupt {
                            message: "legacy graph cleanup lost its replay source".to_owned(),
                        })?;
                (
                    tombstone.key,
                    tombstone.direct_dependency_generations,
                    source,
                    tombstone.expected_recovered_digest,
                )
            };
        if !direct_dependencies.is_empty() {
            return Ok(SealedStagingRelease::Retained(
                SealedStagingRetentionReason::DependencyBearing,
            ));
        }
        let source =
            crate::generation::checked_decode_replay_source(&canonical_replay_source, &|| {
                check_all(&registration, context, "generation.release_sealed_staging")
            })?;
        if !matches!(source, GraphGenerationReplaySource::SealedCodeGeneration(_)) {
            return Ok(SealedStagingRelease::Retained(
                SealedStagingRetentionReason::NoSealedCodeGenerationReplay,
            ));
        }
        let locator = locator_from_key(&key)?;
        // Relational publication evidence is the authority for which sealed
        // artifact may stand in for the staging rows: normally the verified
        // head, or the unique cleanup tombstone for a shipped legacy
        // per-generation projection after its head and replay were retired.
        // The runtime
        // verifies the sealed store's recovered digest against that evidence,
        // opens the staging engine if it is hibernated, releases, and
        // re-hibernates. Requiring an installed lease here left every scope a
        // freshly opened daemon had not activated (only the memory head and
        // the serving generation are) answering NoVerifiedLease forever,
        // which is how a multi-gigabyte staging container accumulated fifteen
        // sealed generations' rows.
        if database.installed_verified_generation(&locator)?.is_none() {
            if relational_head.is_some() {
                let recovered = self.recover_verified_snapshot(
                    registration.clone(),
                    authority,
                    context,
                    projection,
                );
                match recovered {
                    Ok(snapshot) => drop(snapshot),
                    // Recovery may quarantine the generation durably on its
                    // way to this error; a sweep that swallowed it left no
                    // record at the site that asked for it.
                    Err(error) => tracing::warn!(
                        event = "graph_staging_release_recovery_failed",
                        projection = %locator.projection,
                        generation = locator.generation.as_str(),
                        error = %error,
                        "sealed-row release could not recover the verified head; rows stay retained"
                    ),
                }
            } else if let Some(commit) = database.staging_generation_commit(&locator)? {
                let identity = GraphGenerationManifestIdentity::new(
                    locator.projection.clone(),
                    locator.generation.clone(),
                    commit.source_generation,
                    commit.watermark,
                    Vec::new(),
                );
                database.open_sealed_generation_store_if_present(
                    &identity,
                    &relational_recovered_digest,
                )?;
            }
        }
        if let Some(installed) = database.installed_verified_generation(&locator)? {
            database.open_installed_sealed_generation_store_if_present(&installed)?;
        }
        database.release_sealed_generation_staging_rows_with(
            &locator,
            Some(relational_recovered_digest.as_str()),
            &|| check_all(&registration, context, "generation.release_sealed_staging"),
        )
    }

    /// Retire one replay after its sealed code generation has been deleted.
    ///
    /// A matching per-generation projection head is reclaimable because the
    /// deleted code index generation can no longer republish it. Pending
    /// publication, dependency, live-snapshot, known-generation, direct-sealed
    /// reader, and concurrent-retirement guards still retain the native rows.
    #[hotpath::measure(label = "graph_db.replay_pool.retire", impl_type = "GraphDbRegistry")]
    pub fn retire_one_code_generation_replay(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        generation: &tracedecay_domain::CodeGenerationId,
        sealed_state_digest: &crate::SealedGraphStateDigest,
    ) -> Result<GraphReplayCollectionOutcome, GraphDbError> {
        check_all(&registration, context, "generation.publication")?;
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
        let mut heads = BTreeMap::new();
        let mut candidates = Vec::new();
        let mut retired_cleanup = Vec::new();
        let mut sealed_digest_mismatch = false;
        for projection in projections {
            if let Some(head) = authority
                .verified_head(&projection, context)
                .map_err(map_publication_error)?
            {
                heads.insert(locator_from_key(&head.key)?, head);
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
                        &|| check_all(&registration, context, "generation.publication"),
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
                            check_all(&registration, context, "generation.publication")
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
            tracing::warn!(
                event = "graph_replay_retire_digest_mismatch",
                generation = generation.as_str(),
                "a journaled replay for this code generation carries a different sealed \
                 digest; retirement conflicts"
            );
            return Err(GraphDbError::conflict(
                "publication.retire_one_code_generation_replay",
            ));
        }
        let candidate_locators = candidates
            .iter()
            .map(|(locator, _, _)| locator.clone())
            .collect::<BTreeSet<_>>();
        retained.extend(
            heads
                .keys()
                .filter(|locator| !candidate_locators.contains(*locator))
                .cloned(),
        );
        if candidates.is_empty() {
            for (locator, _) in retired_cleanup {
                if matches!(
                    database.delete_generation_contents(&locator, &|| {
                        check_registration_request(&registration, "publication.retired_cleanup")
                    })?,
                    GenerationContentsDeletion::RetentionPending
                ) {
                    tracing::info!(
                        event = "graph_replay_retirement_pending",
                        generation = generation.as_str(),
                        "retired cleanup deferred until the staging engine is already open"
                    );
                    return Ok(GraphReplayCollectionOutcome::RetentionPending);
                }
            }
            return Ok(GraphReplayCollectionOutcome::Absent);
        }
        let selected = {
            let mut state = database.wait_verified_generations_write()?;
            for head in state.heads.values() {
                if candidate_locators.contains(&head.locator) && Arc::strong_count(head) == 1 {
                    // Once the code index deletes a per-generation head, the
                    // registry's installed pointer is not reader liveness.
                    // Its dependency closure remains protected; any snapshot
                    // clone raises the count and retains the whole lease.
                    for dependency in head.dependencies.values() {
                        retain_lease_closure(dependency, &mut retained);
                    }
                } else {
                    retain_lease_closure(head, &mut retained);
                }
            }
            for (locator, weak) in &state.known {
                let installed_candidate_without_reader = candidate_locators.contains(locator)
                    && weak.strong_count() == 1
                    && state.heads.values().any(|head| head.locator == *locator);
                if !installed_candidate_without_reader && weak.upgrade().is_some() {
                    retained.insert(locator.clone());
                }
            }
            // Direct-sealed readers bypass this database's generation state
            // entirely; their liveness is tracked on the registry.
            retained.extend(self.live_direct_sealed_locators()?);
            let selected = candidates
                .into_iter()
                .find(|(locator, _, _)| !retained.contains(locator));
            if let Some((locator, _, _)) = &selected {
                state.retiring.insert(locator.clone());
            }
            selected
        };
        let Some((locator, replay, source)) = selected else {
            tracing::debug!(
                event = "graph_replay_retirement_retained",
                generation = generation.as_str(),
                "every replay for this code generation is still referenced; nothing retires"
            );
            return Ok(GraphReplayCollectionOutcome::Retained);
        };
        tracing::debug!(
            event = "graph_replay_retirement_selected",
            generation = generation.as_str(),
            graph_generation = %locator.generation,
            replay_sequence = replay.sequence.get(),
            "unreferenced sealed code-generation replay selected for retirement"
        );
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
        let selected_head = heads.get(&locator);
        let retirement_outcome = match selected_head {
            Some(head) => authority.retire_verified_head_replay(&retirement, head, context),
            None => authority.retire_replay(&retirement, context),
        };
        let retirement_outcome = match retirement_outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                clear_retiring_fence(&database, &locator)?;
                return Err(map_publication_error(error));
            }
        };
        match retirement_outcome {
            GraphReplayRetirementOutcomeV1::Retired(_)
            | GraphReplayRetirementOutcomeV1::ExactReplay(_) => {
                let legacy_layout = is_legacy_per_generation_code_graph_namespace_str(
                    replay.publication.key.projection.namespace.as_str(),
                );
                if selected_head.is_some() {
                    tracing::info!(
                        event = "graph_replay_head_retired",
                        generation = generation.as_str(),
                        graph_generation = %locator.generation,
                        replay_sequence = replay.sequence.get(),
                        legacy_layout,
                        "verified per-generation graph replay head retired"
                    );
                }
                if legacy_layout {
                    // Migration evidence for issue #836: this projection was
                    // written under the retired per-generation namespace, so
                    // reclaiming it is the explicit drain of pre-cutover
                    // persisted state, not ordinary supersession.
                    tracing::info!(
                        event = "graph_legacy_code_graph_projection_retired",
                        generation = generation.as_str(),
                        graph_generation = %locator.generation,
                        namespace = replay.publication.key.projection.namespace.as_str(),
                        head_retired = selected_head.is_some(),
                        "reclaimed a code-graph projection persisted under the retired \
                         per-generation namespace layout"
                    );
                }
                // Retirement is the linearization point. A failure after it
                // may leak derived bytes, but cannot destroy the source of an
                // active relational replay. A hibernated engine must not be
                // opened here: keep the queue entry and finish native delete
                // on a later tick that already holds the engine open.
                let deletion = match database.delete_generation_contents(&locator, &|| {
                    check_registration_request(&registration, "publication.replay_retirement")
                }) {
                    Ok(deletion) => deletion,
                    Err(error) => {
                        clear_retiring_fence(&database, &locator)?;
                        return Err(error);
                    }
                };
                if matches!(deletion, GenerationContentsDeletion::RetentionPending) {
                    clear_retiring_fence(&database, &locator)?;
                    tracing::info!(
                        event = "graph_replay_retirement_pending",
                        generation = generation.as_str(),
                        graph_generation = %locator.generation,
                        "native row delete deferred until the staging engine is already open"
                    );
                    return Ok(GraphReplayCollectionOutcome::RetentionPending);
                }
                Ok(GraphReplayCollectionOutcome::Retired(Box::new(source)))
            }
            GraphReplayRetirementOutcomeV1::CurrentVerifiedHead { .. }
            | GraphReplayRetirementOutcomeV1::PendingReplay { .. } => {
                clear_retiring_fence(&database, &locator)?;
                Ok(GraphReplayCollectionOutcome::Retained)
            }
            GraphReplayRetirementOutcomeV1::Conflict => {
                clear_retiring_fence(&database, &locator)?;
                tracing::warn!(
                    event = "graph_replay_retirement_conflict",
                    generation = generation.as_str(),
                    graph_generation = %locator.generation,
                    replay_sequence = replay.sequence.get(),
                    "relational replay retirement conflicted with a concurrent authority change"
                );
                Err(GraphDbError::conflict(
                    "publication.retire_one_code_generation_replay",
                ))
            }
            GraphReplayRetirementOutcomeV1::Missing => {
                clear_retiring_fence(&database, &locator)?;
                Err(GraphDbError::Corrupt {
                    message: "graph replay disappeared during exact retirement".to_owned(),
                })
            }
        }
    }

    /// Discard one interrupted publication: the journaled pending replay row
    /// a dead publisher can never complete, plus whatever partial native
    /// generation contents its interrupted staging left behind. The target
    /// is named by the exact observed record (compare-and-swap shaped), so a
    /// row that completed, was superseded, or was re-journaled since the
    /// diagnosis is refused with its evidence instead of deleted. On
    /// `Discarded` the journal position is open again and a fresh replay for
    /// the same generation can be journaled and published (issue #765).
    #[hotpath::measure(
        label = "graph_db.generation.discard_interrupted",
        impl_type = "GraphDbRegistry"
    )]
    pub fn discard_interrupted_publication(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        pending: &GraphPublicationReplayRecordV1,
    ) -> Result<GraphPendingReplayDiscardOutcomeV1, GraphDbError> {
        let operation = self.registered_operation(registration.clone())?;
        operation.check(self, context)?;
        require_projection_binding(&registration, &pending.publication.key.projection)?;
        let database = operation.database().clone();
        let locator = locator_from_key(&pending.publication.key)?;
        // Fence the generation against concurrent recover/open while its
        // journal row and stored rows are removed, and refuse while anything
        // live still retains it: a retained pending generation means an
        // in-flight publisher on this instance still holds its proof lease.
        {
            let mut state = database.wait_verified_generations_write()?;
            if state.retains(&locator) || state.retiring.contains(&locator) {
                return Err(GraphDbError::conflict(
                    "publication.discard_interrupted.generation_retained",
                ));
            }
            state.retiring.insert(locator.clone());
        }
        let request = GraphPendingReplayDiscardV1 {
            key: pending.publication.key.clone(),
            sequence: pending.sequence,
        };
        let outcome = match authority.discard_pending_replay(&request, context) {
            Ok(outcome) => outcome,
            Err(error) => {
                clear_retiring_fence(&database, &locator)?;
                return Err(map_publication_error(error));
            }
        };
        if matches!(outcome, GraphPendingReplayDiscardOutcomeV1::Discarded(_)) {
            // The journal discard is the linearization point. A failure here
            // may leak partial staged rows, but the reopened journal position
            // means the next publication of this generation restages them.
            let deletion = match database.delete_generation_contents(&locator, &|| {
                check_registration_request(&registration, "publication.interrupted_discard")
            }) {
                Ok(deletion) => deletion,
                Err(error) => {
                    clear_retiring_fence(&database, &locator)?;
                    return Err(error);
                }
            };
            if matches!(deletion, GenerationContentsDeletion::RetentionPending) {
                tracing::info!(
                    event = "graph_interrupted_publication_cleanup_pending",
                    generation = %locator.generation,
                    reason = "staging_engine_hibernated",
                    "interrupted publication rows remain for a later open cleanup"
                );
            }
        }
        clear_retiring_fence(&database, &locator)?;
        Ok(outcome)
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
        check_all(&registration, context, "generation.publication")?;
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
                                check_all(&registration, context, "generation.publication")
                            })?;
                        if let GraphGenerationReplaySource::SealedCodeGeneration(source) = source
                            && &source.generation == generation
                        {
                            if &source.sealed_state_digest != sealed_state_digest {
                                tracing::warn!(
                                    event = "graph_replay_cleanup_digest_mismatch",
                                    generation = generation.as_str(),
                                    "retired cleanup for this code generation carries a \
                                     different sealed digest; finalization conflicts"
                                );
                                return Err(GraphDbError::conflict(
                                    "publication.finalize_one_code_generation_replay_cleanup",
                                ));
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
                                    tracing::warn!(
                                        event = "graph_replay_cleanup_finalize_conflict",
                                        generation = generation.as_str(),
                                        "retired replay cleanup finalization conflicted"
                                    );
                                    Err(GraphDbError::conflict(
                                        "publication.finalize_one_code_generation_replay_cleanup",
                                    ))
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
            },
        )
    }

    /// Runs the gateless phase of one verified publication — replay
    /// resolution, native staging, the sealed-store build, and the durable
    /// recovered-digest proof — without advancing the relational verified
    /// head or installing a read-side lease.
    ///
    /// A publication that turns out to be already durably linearized
    /// (an idempotent replay of the current head, or superseded history)
    /// settles here with its snapshot; otherwise the returned proof completes
    /// through [`Self::complete_verified_publication`], which is the only
    /// phase a publication gate needs to cover.
    pub fn prepare_verified_publication(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
        supplied_manifest: Option<Arc<GraphGenerationManifest>>,
    ) -> Result<GraphPublicationPreparationV1, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        self.prepare_verified_publication_inner(
            &operation,
            authority,
            context,
            publication_key,
            GraphPublishModeV1 {
                supplied_manifest,
                reopen_metadata: false,
            },
        )
    }

    /// Completes a prepared publication: the relational verified-head
    /// compare-and-swap and the read-side lease install.
    ///
    /// The proof's retained lease must still name the current registry owner
    /// of the same mounted instance; a store retired and remounted between
    /// the phases answers a typed conflict, and the caller re-prepares.
    pub fn complete_verified_publication(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        proven: ProvenGraphPublicationV1,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let operation = self.registered_operation(registration)?;
        self.complete_verified_publication_inner(&operation, authority, context, proven)
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
        match self.prepare_verified_publication_inner(
            operation,
            authority,
            context,
            publication_key,
            mode,
        )? {
            GraphPublicationPreparationV1::Settled(commit) => Ok(*commit),
            GraphPublicationPreparationV1::Proven(proven) => {
                self.complete_verified_publication_inner(operation, authority, context, *proven)
            }
        }
    }

    /// The gateless phase of one verified publication: replay resolution,
    /// native staging, the sealed-store build, and the durable
    /// recovered-digest proof. Nothing here advances the relational verified
    /// head or installs a read-side lease, so a caller that serializes
    /// publication against readers holds its gate only across
    /// [`Self::complete_verified_publication`].
    #[hotpath::measure(
        label = "graph_db.generation.publish.prepare",
        impl_type = "GraphDbRegistry"
    )]
    fn prepare_verified_publication_inner(
        &self,
        operation: &RegisteredGraphDbOperationV1,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        publication_key: &GraphPublicationKeyV1,
        mode: GraphPublishModeV1,
    ) -> Result<GraphPublicationPreparationV1, GraphDbError> {
        let GraphPublishModeV1 {
            supplied_manifest,
            reopen_metadata,
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
            GraphPublicationReplayLookupV1::Retired(tombstone) => {
                tracing::warn!(
                    event = "graph_publication_replay_retired_conflict",
                    generation = tombstone.key.generation.as_str(),
                    retired_sequence = tombstone.sequence.get(),
                    "the journaled replay for this publication key is retired; \
                     publication conflicts before any journal write"
                );
                return Err(GraphDbError::conflict("publication.prepare.replay_retired"));
            }
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
            let relation = publication_head_relation(current.as_ref(), &replay, &historical_head);
            if matches!(
                relation,
                PublicationHeadRelationV1::OwnLinearizedHead
                    | PublicationHeadRelationV1::SupersededHistory
            ) {
                let is_current_head =
                    matches!(relation, PublicationHeadRelationV1::OwnLinearizedHead);
                let historical_head = if is_current_head {
                    current.clone().ok_or_else(|| GraphDbError::Corrupt {
                        message: "linearized graph publication head disappeared during seating"
                            .to_owned(),
                    })?
                } else {
                    historical_head
                };
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
                // proof below, and a quarantined generation is deliberately
                // a cache miss here: this publication re-projects the rows
                // and re-proves the digest, which is the repair the
                // quarantine was waiting for.
                if let Some(lease) = database.republishable_verified_generation(&locator)?
                    && lease.head == historical_head
                {
                    operation.check(self, context)?;
                    let commit = database.generation_commit(&locator)?.ok_or_else(|| {
                        GraphDbError::GenerationMismatch {
                            namespace: locator.projection.namespace.to_string(),
                            projection: locator.projection.projection.to_string(),
                            generation: locator.generation.to_string(),
                            message: "verified generation rows disappeared under a live lease"
                                .to_owned(),
                        }
                    })?;
                    let recovered_digest = historical_head.recovered_digest.clone();
                    return seat_historical_verified_lease(
                        database,
                        lease,
                        historical_head,
                        is_current_head,
                        commit,
                        recovered_digest,
                    )
                    .map(|commit| GraphPublicationPreparationV1::Settled(Box::new(commit)));
                }
                // A durable projection quarantine blocks dependency hydration
                // too: a dependency walk is allowed to inspect only readable
                // generations. Repair the adopted head first, then load its
                // closure against the reopened, verified projection.
                let quarantined = quarantined_generation_requires_repair(&database, &identity)?;
                let mut visiting = BTreeSet::new();
                let dependencies = if quarantined {
                    BTreeMap::new()
                } else {
                    self.load_dependencies(
                        operation,
                        &database,
                        authority,
                        context,
                        &identity,
                        &mut visiting,
                    )?
                };
                // The journaled replay's expected recovered digest already
                // binds this exact manifest, so the durable generation proof
                // checks against it directly instead of re-canonicalizing the
                // full manifest.
                let sealed_digest = &replay.publication.expected_recovered_digest;
                let row_counts = (entity_rows, relation_rows);
                if quarantined {
                    if !is_current_head {
                        // A projection-level quarantine must only be cleared
                        // by the exact verified head that adopted it. A
                        // superseded generation cannot establish that
                        // ownership, even when its stored rows still prove.
                        return Err(GraphDbError::conflict_observed(
                            "publication.repair.adopted_head",
                            describe_verified_head(Some(&historical_head)),
                            describe_verified_head(current.as_ref()),
                        ));
                    }
                    // The initial corruption path records a durable
                    // projection quarantine, which correctly bars ordinary
                    // staging. First re-open and fully verify the exact
                    // adopted head. A sealed-only code generation has already
                    // released its duplicate staging rows, though: that proof
                    // truthfully reports a mismatch. Only then may the exact
                    // current head restore those rows from its canonical
                    // replay before retrying the checkpointed proof.
                    let repair_authority = RefCell::new(authority);
                    let repair_check = || {
                        operation.check(self, context)?;
                        let mut authority = repair_authority.try_borrow_mut().map_err(|_| {
                            GraphDbError::conflict("publication.repair.authority_reentrancy")
                        })?;
                        let observed = authority
                            .verified_head(&historical_head.key.projection, context)
                            .map_err(map_publication_error)?;
                        if observed.as_ref() != Some(&historical_head) {
                            return Err(GraphDbError::conflict_observed(
                                "publication.repair.adopted_head",
                                describe_verified_head(Some(&historical_head)),
                                describe_verified_head(observed.as_ref()),
                            ));
                        }
                        let replay = authority
                            .replay(&historical_head.key, context)
                            .map_err(map_publication_error)?;
                        let replay = require_active_replay_evidence(
                            replay,
                            "adopted graph head has no durable active replay",
                        )?;
                        require_head_replay(&historical_head, &replay)
                    };
                    let repaired = database.reopen_and_verify_existing_generation(
                        &identity,
                        sealed_digest,
                        row_counts,
                        &repair_check,
                    );
                    let (historical_commit, recovered_digest) = match repaired {
                        Ok(recovered) => recovered,
                        Err(GraphDbError::GenerationMismatch { .. })
                            if identity.dependencies.is_empty() =>
                        {
                            // Keep the durable marker in place while the
                            // canonical pages are restored. This process-local
                            // sealed-only lease only opens the existing exact
                            // staging path; readers still fail closed on the
                            // durable quarantine until the second full proof
                            // clears it through the checkpoint authority.
                            let repair_lease = generation_lease(
                                &identity,
                                historical_head.clone(),
                                BTreeMap::new(),
                            );
                            database.remember_sealed_only_generation(&repair_lease)?;
                            database.apply_generation_unverified_with_digest_observed(
                                manifest,
                                sealed_digest,
                                &repair_check,
                            )?;
                            database.reopen_and_verify_existing_generation(
                                &identity,
                                sealed_digest,
                                row_counts,
                                &repair_check,
                            )?
                        }
                        Err(error) => return Err(error),
                    };
                    // Quarantine invalidates the derived sealed artifact.
                    // Rebuild it only after the checkpointed staging proof
                    // has cleared the adopted marker.
                    database.ensure_sealed_generation_store(
                        &identity,
                        sealed_digest,
                        &repair_check,
                    )?;
                    let authority = repair_authority.into_inner();
                    let dependencies = self.load_dependencies(
                        operation,
                        &database,
                        authority,
                        context,
                        &identity,
                        &mut visiting,
                    )?;
                    operation.check(self, context)?;
                    let lease = generation_lease(&identity, historical_head.clone(), dependencies);
                    return seat_historical_verified_lease(
                        database,
                        lease,
                        historical_head,
                        true,
                        historical_commit,
                        recovered_digest,
                    )
                    .map(|commit| GraphPublicationPreparationV1::Settled(Box::new(commit)));
                }
                let (historical_commit, recovered_digest) =
                    match (apply_native, has_supplied_manifest) {
                        (true, _) => {
                            let staged = database
                                .apply_generation_unverified_with_digest_observed(
                                    manifest,
                                    sealed_digest,
                                    &check,
                                )?;
                            match staged {
                                GenerationStageOutcome::Applied(commit) => {
                                    // A repair that wrote missing native rows is
                                    // a new seal: build and prove its derived
                                    // artifact before seating it.
                                    let (_, recovered) = database
                                        .verify_generation_for_publication(
                                            &identity,
                                            sealed_digest,
                                            row_counts,
                                            true,
                                            &check,
                                        )?;
                                    (commit, recovered)
                                }
                                GenerationStageOutcome::Reseated(commit) => {
                                    // An already-complete generation is an
                                    // activation. Verify the staging authority
                                    // and adopt an existing sealed artifact, but
                                    // never construct a missing whole-generation
                                    // copy before the rows can serve.
                                    let recovered = database.verify_activated_generation(
                                        &identity,
                                        sealed_digest,
                                        &check,
                                    )?;
                                    database.open_sealed_generation_store_if_present(
                                        &identity,
                                        sealed_digest,
                                    )?;
                                    (commit, recovered)
                                }
                            }
                        }
                        (false, true) => {
                            drop(manifest);
                            database.verify_generation_for_publication(
                                &identity,
                                sealed_digest,
                                row_counts,
                                true,
                                &check,
                            )?
                        }
                        (false, false) if reopen_metadata => {
                            drop(manifest);
                            database.verify_generation_for_publication(
                                &identity,
                                sealed_digest,
                                row_counts,
                                true,
                                &check,
                            )?
                        }
                        (false, false) => {
                            drop(manifest);
                            database.verify_generation_for_publication(
                                &identity,
                                sealed_digest,
                                row_counts,
                                false,
                                &check,
                            )?
                        }
                    };
                operation.check(self, context)?;
                // The digest proof above already streamed this generation's
                // stored rows against the head's journaled recovered digest
                // on this instance (`historical_head` carries exactly
                // `expected_recovered_digest`), and the dependency closure
                // was validated when it was loaded, so the verified lease is
                // built directly from that proof. Re-loading the head
                // through its replay would hydrate the manifest and stream
                // the rows a second time without adding durability. Seating
                // records this fresh proof and clears any prior quarantine;
                // the read-side quarantine guard must remain closed until
                // that proof-bearing lease is installed.
                let lease = generation_lease(&identity, historical_head.clone(), dependencies);
                return seat_historical_verified_lease(
                    database,
                    lease,
                    historical_head,
                    is_current_head,
                    historical_commit,
                    recovered_digest,
                )
                .map(|commit| GraphPublicationPreparationV1::Settled(Box::new(commit)));
            }
            let expected = replay.publication.expected_prior_head.as_ref();
            tracing::warn!(
                event = "graph_publication_prior_head_conflict",
                generation = replay.publication.key.generation.as_str(),
                replay_sequence = replay.sequence.get(),
                expected_prior_generation =
                    expected.map_or("none", |head| head.key.generation.as_str()),
                expected_prior_sequence = expected.map_or(0, |head| head.sequence.get()),
                current_generation = current
                    .as_ref()
                    .map_or("none", |head| head.key.generation.as_str()),
                current_sequence = current.as_ref().map_or(0, |head| head.sequence.get()),
                "journaled replay expects a different prior verified head and the current \
                 head is not newer; publication conflicts before the verified-head CAS"
            );
            return Err(GraphDbError::conflict_observed(
                "publication.prepare.expected_prior_head",
                describe_verified_head(replay.publication.expected_prior_head.as_ref()),
                describe_verified_head(current.as_ref()),
            ));
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

        // The journaled replay's expected recovered digest already binds this
        // exact manifest (inline decode, sealed identity pin, or supplied-
        // manifest binding above), so the durable generation proof checks
        // against it directly instead of re-canonicalizing the full manifest
        // a second time.
        let sealed_digest = &replay.publication.expected_recovered_digest;
        let row_counts = (entity_rows, relation_rows);
        let verified = match (apply_native, has_supplied_manifest) {
            // A supplied manifest for a metadata-only replay carries the
            // native rows (vectors) the canonical source omits; a first
            // commit must install them natively before verification.
            //
            // Staging consumes the manifest and releases its bulk rows at the
            // last durable page commit, so the artifact proof below runs
            // without them resident.
            (true, _) | (false, true) => {
                let staged = database.apply_generation_unverified_with_digest_observed(
                    manifest,
                    sealed_digest,
                    &check,
                )?;
                let commit = staged.commit();
                database
                    .verify_generation_for_publication(
                        &identity,
                        sealed_digest,
                        row_counts,
                        true,
                        &check,
                    )
                    .map(|(_, recovered)| (commit, recovered))
            }
            (false, false) if reopen_metadata => {
                drop(manifest);
                database.verify_generation_for_publication(
                    &identity,
                    sealed_digest,
                    row_counts,
                    true,
                    &check,
                )
            }
            (false, false) => {
                drop(manifest);
                database.verify_generation_for_publication(
                    &identity,
                    sealed_digest,
                    row_counts,
                    false,
                    &check,
                )
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
        operation.check(self, context)?;
        Ok(GraphPublicationPreparationV1::Proven(Box::new(
            ProvenGraphPublicationV1 {
                database,
                identity,
                dependencies,
                commit,
                recovered_digest,
                replay,
            },
        )))
    }

    /// The linearization phase of one verified publication: the relational
    /// verified-head compare-and-swap and the read-side lease install. This
    /// is the only publication phase that changes what readers observe, so a
    /// caller that gates publication against reads holds its gate across
    /// exactly this call.
    #[hotpath::measure(
        label = "graph_db.generation.publish.complete",
        impl_type = "GraphDbRegistry"
    )]
    fn complete_verified_publication_inner(
        &self,
        operation: &RegisteredGraphDbOperationV1,
        authority: &mut dyn GraphPublicationStoreV1,
        context: &GraphPublicationOperationContextV1<'_>,
        proven: ProvenGraphPublicationV1,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        let ProvenGraphPublicationV1 {
            database,
            identity,
            dependencies,
            commit,
            recovered_digest,
            replay,
        } = proven;
        operation.check(self, context)?;
        operation.require_publication_binding(&replay.publication.key)?;
        // The proof is bound to the exact mounted instance it streamed; a
        // completion arriving through a different instance of the same store
        // must not install it.
        if !database.shares_runtime_with(operation.database()) {
            tracing::warn!(
                event = "graph_publication_instance_conflict",
                generation = replay.publication.key.generation.as_str(),
                "publication proof was bound to a different mounted graph instance; \
                 completion conflicts"
            );
            return Err(GraphDbError::conflict(
                "publication.complete.foreign_instance",
            ));
        }
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
            GraphVerifiedHeadCasOutcomeV1::Conflict { actual } => {
                if let Some(head) = actual
                    .as_ref()
                    .filter(|head| head.key == cas.publication_key)
                    .cloned()
                {
                    // The live head is this publication: a twin publisher
                    // linearized the same journal, or the CAS observed its
                    // own write as a conflict. Seat that head instead of
                    // looping Conflict on the incumbent.
                    let lease = generation_lease(&identity, head.clone(), dependencies);
                    database.install_verified_generation(Arc::clone(&lease))?;
                    database.record_memory_checkpoint(
                        crate::hotpath_observe::GrafeoMemoryPhase::Published,
                    );
                    let mut closure = BTreeMap::new();
                    collect_closure(&lease, &mut closure)?;
                    return Ok(VerifiedGraphCommit {
                        commit,
                        recovered_digest: head.recovered_digest.clone(),
                        head,
                        snapshot: VerifiedGraphSnapshot::new(database, lease, closure),
                    });
                }
                let expected = cas.expected_prior_head.as_ref();
                tracing::warn!(
                    event = "graph_publication_cas_conflict",
                    generation = cas.publication_key.generation.as_str(),
                    expected_prior_generation =
                        expected.map_or("none", |head| head.key.generation.as_str()),
                    expected_prior_sequence = expected.map_or(0, |head| head.sequence.get()),
                    actual_generation = actual
                        .as_ref()
                        .map_or("none", |head| head.key.generation.as_str()),
                    actual_sequence = actual.as_ref().map_or(0, |head| head.sequence.get()),
                    "relational verified-head compare-and-swap observed a different \
                     current head"
                );
                return Err(GraphDbError::conflict_observed(
                    "publication.complete.cas_prior_head",
                    describe_verified_head(cas.expected_prior_head.as_ref()),
                    describe_verified_head(actual.as_ref()),
                ));
            }
            GraphVerifiedHeadCasOutcomeV1::ReplayInputConflict { existing } => {
                tracing::warn!(
                    event = "graph_publication_cas_replay_input_conflict",
                    generation = cas.publication_key.generation.as_str(),
                    existing_sequence = existing.sequence.get(),
                    "relational verified-head compare-and-swap found the journaled \
                     replay bound to different inputs"
                );
                return Err(GraphDbError::conflict_observed(
                    "publication.complete.cas_replay_inputs",
                    format!("replay input {}", cas.input_digest.as_str()),
                    format!(
                        "journaled input {}",
                        existing.publication.input_digest.as_str()
                    ),
                ));
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
            GraphVerifiedHeadCasOutcomeV1::RetiredReplay(tombstone) => {
                tracing::warn!(
                    event = "graph_publication_cas_retired_replay",
                    generation = tombstone.key.generation.as_str(),
                    retired_sequence = tombstone.sequence.get(),
                    "the journaled replay was retired before the verified-head \
                     compare-and-swap"
                );
                return Err(GraphDbError::conflict(
                    "publication.complete.replay_retired",
                ));
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
            GraphPublicationReplayLookupV1::Retired(_) => {
                return Err(GraphDbError::conflict(
                    "publication.snapshot.replay_retired",
                ));
            }
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
            tracing::debug!(
                event = "graph_generation_snapshot_superseded",
                generation = replay.publication.key.generation.as_str(),
                replay_sequence = replay.sequence.get(),
                current_generation = current.key.generation.as_str(),
                current_sequence = current.sequence.get(),
                "exact verified generation lookup conflicts with the current verified head"
            );
            return Err(GraphDbError::conflict_observed(
                "publication.snapshot.replay_ahead_of_head",
                format!("head seq >= {}", replay.sequence.get()),
                describe_verified_head(Some(&current)),
            ));
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

    #[hotpath::measure(
        label = "graph_db.generation.load_dependencies",
        impl_type = "GraphDbRegistry"
    )]
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
                .ok_or_else(|| {
                    tracing::warn!(
                        event = "graph_dependency_head_missing",
                        dependency_generation = %dependency.generation,
                        "dependency projection has no relational verified head; \
                         publication conflicts"
                    );
                    GraphDbError::conflict("publication.dependencies.missing_verified_head")
                })?;
            if relational_head.sequence < replay.sequence {
                tracing::warn!(
                    event = "graph_dependency_head_stale",
                    dependency_generation = %dependency.generation,
                    head_sequence = relational_head.sequence.get(),
                    replay_sequence = replay.sequence.get(),
                    "dependency verified head is older than its replay; publication conflicts"
                );
                return Err(GraphDbError::conflict_observed(
                    "publication.dependencies.replay_ahead_of_head",
                    format!("head seq >= {}", replay.sequence.get()),
                    describe_verified_head(Some(&relational_head)),
                ));
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

    #[hotpath::measure(
        label = "graph_db.generation.load_verified_head",
        impl_type = "GraphDbRegistry"
    )]
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
        if database.is_sealed_only_generation(&locator)? {
            let sealed = database.sealed_generation_reader(&locator).ok_or_else(|| {
                GraphDbError::ResetRequired {
                    message: "sealed-only graph generation lost its serving artifact; republish from the canonical manifest".to_owned(),
                }
            })?;
            if sealed.recovered_digest() != head.recovered_digest.as_str() {
                return Err(GraphDbError::ResetRequired {
                    message: "sealed-only graph generation artifact no longer matches its relational head; republish from the canonical manifest".to_owned(),
                });
            }
        }
        if let Some(lease) = database.verified_generation(&locator)?
            && lease.head == head
            && (!database.is_sealed_only_generation(&locator)?
                || database.sealed_generation_reader(&locator).is_some())
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
        let sealed_code_generation = matches!(
            crate::generation::checked_decode_replay_source(
                &replay.publication.canonical_replay_source,
                &check,
            )?,
            GraphGenerationReplaySource::SealedCodeGeneration(_)
        );
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
        if let Some(lease) = database.verified_generation(&locator)?
            && (!database.is_sealed_only_generation(&locator)?
                || database.sealed_generation_reader(&locator).is_some())
        {
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
        if sealed_code_generation && !database.staging_generation_has_rows(&locator)? {
            if !dependencies.is_empty() {
                return Err(GraphDbError::ResetRequired {
                    message: "dependency-bearing graph generation lost its staging rows; republish from the canonical manifest".to_owned(),
                });
            }
            database.open_sealed_generation_store_if_present(&identity, &head.recovered_digest)?;
            let sealed = database.sealed_generation_reader(&locator).ok_or_else(|| {
                GraphDbError::ResetRequired {
                    message: "graph generation has neither staging rows nor a usable sealed artifact; republish from the canonical manifest".to_owned(),
                }
            })?;
            if sealed.recovered_digest() != head.recovered_digest.as_str() {
                return Err(GraphDbError::ResetRequired {
                    message: "sealed-only graph generation artifact no longer matches its relational head; republish from the canonical manifest".to_owned(),
                });
            }
            let lease = generation_lease(&identity, head, BTreeMap::new());
            database.remember_sealed_only_generation(&lease)?;
            visiting.remove(&locator);
            return Ok(lease);
        }
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
        // `require_head_replay` pinned this head to its journaled replay, and
        // the manifest was proven to bind that replay's digests when it was
        // decoded above, so the stored rows verify directly against the head
        // digest without canonicalizing the full manifest a second time.
        // Through the marker-aware entry point, not the free function: an
        // open over container bytes an earlier open already proved resolves
        // by stat instead of re-streaming every row, and either outcome is
        // recorded. Corruption still fails here exactly as the full proof
        // would - `expected` comes from the relational head, never a marker.
        match database.verify_activated_generation(&identity, &head.recovered_digest, &check) {
            Ok(_) => {}
            Err(error @ GraphDbError::GenerationMismatch { .. }) => {
                database.quarantine_generation(&identity)?;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
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

/// How a journaled replay relates to the live verified head.
///
/// Full-struct equality is not the linearization: a remount can reconstruct
/// the same publication key with a drifted digest field. Foreign and newer
/// markers stay conflicts so recovery never clears a marker it did not adopt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationHeadRelationV1 {
    /// Live head matches the journaled prior; first-publish apply may proceed.
    ExpectedPrior,
    /// This journaled publication already owns the head (CAS linearized it).
    OwnLinearizedHead,
    /// A later publication already won; seat this replay as history.
    SupersededHistory,
    /// A foreign or incomparable marker occupies the projection.
    ForeignMarker,
}

/// Whether this exact generation is still refused by the read-side
/// quarantine guard. The guard's mismatch is deliberately distinct from a
/// retirement or lease conflict: only a full replay of the adopted current
/// head can repair it.
fn quarantined_generation_requires_repair(
    database: &GraphDbLeaseV1,
    identity: &GraphGenerationManifestIdentity,
) -> Result<bool, GraphDbError> {
    let namespace = identity.physical_namespace()?;
    match database.ensure_projection_readable(&namespace, &identity.projection.projection) {
        Ok(_) => Ok(false),
        Err(GraphDbError::ProjectionMismatch { .. }) => Ok(true),
        Err(error) => Err(error),
    }
}

fn publication_head_relation(
    current: Option<&GraphVerifiedHeadV1>,
    replay: &GraphPublicationReplayRecordV1,
    historical_head: &GraphVerifiedHeadV1,
) -> PublicationHeadRelationV1 {
    let expected = replay.publication.expected_prior_head.as_ref();
    if current == expected {
        return PublicationHeadRelationV1::ExpectedPrior;
    }
    if current.is_some_and(|head| head.key == replay.publication.key)
        || current == Some(historical_head)
    {
        return PublicationHeadRelationV1::OwnLinearizedHead;
    }
    if current.is_some_and(|head| head.sequence > replay.sequence) {
        return PublicationHeadRelationV1::SupersededHistory;
    }
    PublicationHeadRelationV1::ForeignMarker
}

/// Compact operator-log rendering of one verified-head position, used as the
/// expected/actual evidence in conflict verdicts: the head sequence, the
/// generation it seats, and the input digest sealing its identity.
fn describe_verified_head(head: Option<&GraphVerifiedHeadV1>) -> String {
    match head {
        Some(head) => format!(
            "head seq {} generation `{}` input {}",
            head.sequence.get(),
            head.key.generation,
            head.input_digest.as_str(),
        ),
        None => "no verified head".to_owned(),
    }
}

/// Seats a verified lease for a historical (already durably linearized)
/// publication and assembles its commit receipt.
///
/// The lease is either freshly built from a digest proof this call just ran,
/// or reused from this exact instance's verified-generation cache; both carry
/// the same instance-bound proof, so seating is identical.
#[hotpath::measure(label = "graph_db.generation.seat_historical")]
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
    #[cfg(feature = "graph-sealed-store")]
    use tracedecay_domain::{CodeGenerationId, RepositoryId};
    use tracedecay_store::runtime::GraphReplayRetirementOutcomeV1;
    use tracedecay_store::{
        BrainId, GraphGenerationIdV1, GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1,
        GraphPublicationInputDigestV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
        GraphPublicationProjectionPageRequestV1, GraphPublicationProjectionPageV1,
        GraphPublicationReplayLookupV1, GraphPublicationReplayPageRequestV1,
        GraphPublicationReplayPageV1, GraphPublicationReplayRecordV1,
        GraphPublicationReplayRetirementV1, GraphPublicationReplayV1,
        GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationRetiredCleanupPageV1,
        GraphPublicationSequenceV1, GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1,
        GraphPublicationStoreV1, GraphReplayAppendOutcomeV1,
        GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
        GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1, ProjectId,
        RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
        RetainedGraphStoreOwnerOperationLeaseErrorV1, RuntimeCancellationIdV1,
        RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
        RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
        StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
        UserProfileId, VerifiedStoreLocatorV1, canonical_store_locator_digest,
    };

    use crate::generation::{
        recovered_generation_enumerations, reset_recovered_generation_enumerations,
        reset_sealed_copy_proofs, sealed_copy_proofs,
    };
    #[cfg(feature = "graph-sealed-store")]
    use crate::generation::{reset_sealed_copy_marker_hits, sealed_copy_marker_hits};
    use crate::lease::GenerationLocator;
    use crate::{
        GraphCancellation, GraphDbError, GraphDbOwnerRegistrationV1, GraphDbRegistration,
        GraphDbRegistry, GraphDbRegistryConfig, GraphEntity, GraphEntityId, GraphGenerationId,
        GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace, GraphProjectionId,
        GraphProjectionIdentity, GraphProperty, GraphPropertyName, GraphWatermark,
        SourceGeneration,
    };
    #[cfg(feature = "graph-sealed-store")]
    use crate::{GraphProjectorRevision, SealedCodeGenerationReplay, SealedGraphStateDigest};

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

        fn retire_verified_head_replay(
            &mut self,
            _request: &GraphPublicationReplayRetirementV1,
            _expected_head: &GraphVerifiedHeadV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1> {
            Err(GraphPublicationStoreErrorV1::Infrastructure)
        }

        fn discard_pending_replay(
            &mut self,
            _request: &tracedecay_store::runtime::GraphPendingReplayDiscardV1,
            _context: &GraphPublicationOperationContextV1,
        ) -> GraphPublicationStoreResultV1<
            tracedecay_store::runtime::GraphPendingReplayDiscardOutcomeV1,
        > {
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
                    BTreeMap::from([
                        (
                            GraphPropertyName::new("marker").unwrap(),
                            GraphProperty::String("reuse".to_owned()),
                        ),
                        // A Bytes property matches the serialized-record shape
                        // every production code graph takes, so these reuse
                        // and marker tests exercise the compact artifact form.
                        (
                            GraphPropertyName::new("payload").unwrap(),
                            GraphProperty::Bytes(vec![0x7d, 0x11, 0x03]),
                        ),
                    ]),
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
        // Discard what the fixture's own publication proved; only the
        // operation under test is being counted.
        let _ = crate::take_graph_db_verification_counters();
        reset_sealed_copy_proofs();
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
            if cfg!(feature = "graph-sealed-store") {
                0
            } else {
                1
            },
            "the durable sealed proof must replace the staging proof when available"
        );
        // The sealed per-generation copy is proved after durable reopen,
        // before it can be installed or answer a read.
        assert_eq!(
            sealed_copy_proofs(),
            if cfg!(feature = "graph-sealed-store") {
                1
            } else {
                0
            },
            "a first seal proves the exact durable artifact before installation"
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

    #[cfg(feature = "graph-sealed-store")]
    #[test]
    fn sealed_snapshot_recovers_without_opening_the_staging_registry() {
        let mut fixture = published_fixture();
        // Replace the fixture's inline replay with the production code-graph
        // replay shape. The sealed store was built from this exact manifest,
        // so its recovered digest and physical namespace stay unchanged while
        // the relational source now proves the direct-sealed route is the one
        // exercised below.
        let manifest = test_manifest(fixture.projection.clone());
        let replay = manifest
            .relational_sealed_replay(
                fixture.binding.shard_id.clone(),
                GraphIdempotencyKey::new("publish:reuse-g1").unwrap(),
                GraphPublicationInputDigestV1::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
                None,
                SealedCodeGenerationReplay {
                    repository: RepositoryId::new("repository.publication-reuse").unwrap(),
                    generation: CodeGenerationId::new("code-generation.publication-reuse").unwrap(),
                    sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                        "sha256:{}",
                        "b".repeat(64)
                    ))
                    .unwrap(),
                    projector_revision: GraphProjectorRevision::try_from(
                        "code-graph-projector.publication-reuse".to_owned(),
                    )
                    .unwrap(),
                },
                &|| Ok(()),
            )
            .unwrap();
        let record = GraphPublicationReplayRecordV1::new(fixture.head.sequence, replay).unwrap();
        fixture.key = record.publication.key.clone();
        fixture.head =
            GraphVerifiedHeadV1::from_replay(&record, fixture.head.recovered_digest.clone())
                .unwrap();
        fixture
            .authority
            .records
            .insert(fixture.key.clone(), record);
        fixture
            .authority
            .heads
            .insert(fixture.key.projection.clone(), fixture.head.clone());
        assert!(
            fixture
                .registry
                .close(&registration(fixture.binding.clone(), &fixture.root))
                .unwrap(),
            "the publishing runtime must close before cold direct recovery"
        );
        let unopened = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        let registration = registration(fixture.binding.clone(), &fixture.root);

        assert!(
            !unopened
                .shard_is_registered(&fixture.binding.shard_id)
                .unwrap()
        );
        let snapshot = unopened
            .recover_verified_sealed_snapshot(
                registration,
                &mut fixture.authority,
                &context,
                &fixture.key.projection,
            )
            .expect("the sealed artifact should recover without staging registration");

        assert_eq!(snapshot.verified_head(), &fixture.head);
        assert_eq!(snapshot.generation(), &fixture.generation);
        assert!(snapshot.serves_from_sealed_store());
        assert!(
            snapshot
                .entity(
                    &crate::GraphEntityRef::new(
                        fixture.projection.clone(),
                        crate::GraphEntityId::new("entity:reuse").unwrap(),
                    ),
                    Arc::new(TestCancellation),
                )
                .unwrap()
                .is_some()
        );
        assert!(
            !unopened
                .shard_is_registered(&fixture.binding.shard_id)
                .unwrap()
        );
    }

    #[test]
    fn direct_sealed_recovery_refuses_an_inline_replay() {
        let mut fixture = published_fixture();
        let unopened = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();

        assert!(matches!(
            unopened.recover_verified_sealed_snapshot(
                registration(fixture.binding.clone(), &fixture.root),
                &mut fixture.authority,
                &context,
                &fixture.key.projection,
            ),
            Err(GraphDbError::Unavailable { message })
                if message == "graph replay has no sealed code-generation source"
        ));
        assert!(
            !unopened
                .shard_is_registered(&fixture.binding.shard_id)
                .unwrap()
        );
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
        // Discard what the fixture's own publication proved; only the
        // operation under test is being counted.
        let _ = crate::take_graph_db_verification_counters();
        reset_sealed_copy_proofs();
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
        assert_eq!(
            sealed_copy_proofs(),
            0,
            "the installed sealed store is reused; no sealed copy is rebuilt or re-proven"
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
        // Discard what the fixture's own publication proved; only the
        // operation under test is being counted.
        let _ = crate::take_graph_db_verification_counters();
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
        // Discard what the fixture's own publication proved; only the
        // operation under test is being counted.
        let _ = crate::take_graph_db_verification_counters();
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
        // One proof, whichever kind. Before the duplicate-proof fix this
        // enumerated the stored rows twice; since verify-once, an open over
        // unchanged container bytes settles it by stat instead. Either way the
        // invariant under test is that the work happens exactly once.
        let counters = crate::take_graph_db_verification_counters();
        assert_eq!(
            counters.full_verifications + counters.marker_hits,
            1,
            "a fresh-from-disk republication must prove the generation exactly once, not twice"
        );
        assert_eq!(
            recovered_generation_enumerations(),
            usize::from(counters.full_verifications == 1),
            "row enumeration must happen only when the proof was not inherited"
        );
    }

    /// A remount that recovers the generation adopts the on-disk sealed
    /// artifact through its verify-once marker: the artifact's bytes are the
    /// ones the build's post-reopen proof ran over, so adoption resolves by
    /// stat instead of re-streaming the sealed row proof — which is exactly
    /// the second half of the boot-from-sealed double verification.
    #[cfg(feature = "graph-sealed-store")]
    #[test]
    fn a_fresh_from_disk_recover_adopts_the_sealed_artifact_by_marker() {
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
        reset_sealed_copy_proofs();
        reset_sealed_copy_marker_hits();
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
            sealed_copy_proofs(),
            0,
            "adopting unchanged sealed bytes must not re-stream the sealed row proof"
        );
        assert_eq!(
            sealed_copy_marker_hits(),
            1,
            "the artifact's own verify-once marker resolves the adoption proof by stat"
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
        // Discard what the fixture's own publication proved; only the
        // operation under test is being counted.
        let _ = crate::take_graph_db_verification_counters();
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
        // A fresh-from-disk open must still *prove* the generation before
        // serving it. Since verify-once that proof may be inherited from a
        // marker over the same container bytes rather than re-streamed; what
        // must never happen is serving with no proof at all.
        let counters = crate::take_graph_db_verification_counters();
        assert_eq!(
            counters.full_verifications + counters.marker_hits,
            1,
            "a genuinely fresh-from-disk open must resolve the recovered-digest proof exactly once"
        );
        assert_eq!(
            recovered_generation_enumerations(),
            usize::from(counters.full_verifications == 1),
            "row enumeration must happen only when the proof was not inherited"
        );
    }

    #[test]
    fn corrupt_current_head_repair_clears_its_marker_across_restart() {
        let mut fixture = published_fixture();
        let manifest = Arc::new(test_manifest(fixture.projection.clone()));
        let identity = manifest.identity();
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        let database = fixture
            .registry
            .resolve(registration(fixture.binding.clone(), &fixture.root))
            .unwrap();
        database.quarantine_generation(&identity).unwrap();
        assert!(matches!(
            database.verified_generation(&locator),
            Err(GraphDbError::GenerationMismatch { .. })
        ));
        drop(database);
        assert!(
            fixture
                .registry
                .close(&registration(fixture.binding.clone(), &fixture.root))
                .unwrap(),
            "the quarantine must survive the restart before its exact-head repair"
        );
        mount(&fixture.registry, &fixture.binding, &fixture.root);

        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        let repaired = fixture
            .registry
            .publish_verified(
                registration(fixture.binding.clone(), &fixture.root),
                &mut fixture.authority,
                &context,
                &fixture.key,
                Some(Arc::clone(&manifest)),
            )
            .unwrap();
        assert_eq!(repaired.head, fixture.head);
        assert_eq!(repaired.snapshot.generation(), &fixture.generation);
        assert_eq!(
            fixture.authority.heads[&fixture.key.projection],
            fixture.head
        );
        assert_eq!(
            fixture.authority.next_sequence, 1,
            "repairing a current quarantined head must not mint a successor replay"
        );
        drop(repaired);

        assert!(
            fixture
                .registry
                .close(&registration(fixture.binding.clone(), &fixture.root))
                .unwrap(),
            "the repaired marker must be checkpointed before another restart"
        );
        mount(&fixture.registry, &fixture.binding, &fixture.root);
        let database = fixture
            .registry
            .resolve(registration(fixture.binding.clone(), &fixture.root))
            .unwrap();
        assert!(
            !super::quarantined_generation_requires_repair(&database, &identity).unwrap(),
            "the exact-head repair must checkpoint the durable marker clear"
        );
        drop(database);
        let recovered = fixture
            .registry
            .recover_verified_snapshot(
                registration(fixture.binding.clone(), &fixture.root),
                &mut fixture.authority,
                &context,
                &fixture.key.projection,
            )
            .unwrap();
        assert_eq!(recovered.verified_head(), &fixture.head);
        assert_eq!(recovered.generation(), &fixture.generation);
    }

    #[test]
    fn corrupt_superseded_generation_cannot_clear_the_current_marker() {
        let mut fixture = published_fixture();
        let manifest = Arc::new(test_manifest(fixture.projection.clone()));
        let identity = manifest.identity();
        let database = fixture
            .registry
            .resolve(registration(fixture.binding.clone(), &fixture.root))
            .unwrap();
        database.quarantine_generation(&identity).unwrap();
        drop(database);

        let mut newer_head = fixture.head.clone();
        newer_head.sequence =
            GraphPublicationSequenceV1::new(fixture.head.sequence.get() + 1).unwrap();
        newer_head.key.generation = GraphGenerationIdV1::new("reuse-g2").unwrap();
        newer_head.key.idempotency_key =
            GraphPublicationIdempotencyKeyV1::new("publish:reuse-g2").unwrap();
        fixture
            .authority
            .heads
            .insert(fixture.key.projection.clone(), newer_head);

        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        assert!(matches!(
            fixture.registry.publish_verified(
                registration(fixture.binding.clone(), &fixture.root),
                &mut fixture.authority,
                &context,
                &fixture.key,
                Some(manifest),
            ),
            Err(GraphDbError::Conflict { .. })
        ));
        assert!(
            fixture
                .registry
                .close(&registration(fixture.binding.clone(), &fixture.root))
                .unwrap(),
            "a rejected superseded repair must leave the marker durable"
        );
        mount(&fixture.registry, &fixture.binding, &fixture.root);
        let database = fixture
            .registry
            .resolve(registration(fixture.binding.clone(), &fixture.root))
            .unwrap();
        assert!(super::quarantined_generation_requires_repair(&database, &identity).unwrap());
    }
}
