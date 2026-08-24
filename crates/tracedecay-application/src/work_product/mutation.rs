use std::collections::BTreeMap;

use tracedecay_domain::{
    ActorId, MAX_WORK_PRODUCT_EVENT_EVIDENCE, ManifestDigest, UtcMicros, WorkCommandId,
    WorkGraphChangeV1, WorkGraphVersionV1, WorkProductEventPayloadV1, WorkProductEventV1,
    WorkProductGraphV1, WorkProductProfileScopeV1, WorkProductSourceWatermarkV1,
    WorkProposalDispositionV1, canonical_sha256,
};

use crate::{RequestAdmission, RequestContext};

use super::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkGraphReadPortV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkProductApplicationErrorV1, WorkProductBindingV1,
    WorkProductOwnerAuthorizationErrorV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductPortContextV1, WorkProductSelectionScopeV1,
};

mod contracts;
pub use contracts::*;

const WORK_PRODUCT_MUTATION_DIGEST_DOMAIN: &str =
    "tracedecay.application.work-product-mutation.final-v2";

pub struct WorkProductMutationServiceV1<G, A, E> {
    graph: G,
    owner_authority: A,
    events: E,
}

impl<G, A, E> WorkProductMutationServiceV1<G, A, E>
where
    G: WorkGraphReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
    E: WorkProductEventPortV1,
{
    pub const fn new(graph: G, owner_authority: A, events: E) -> Self {
        Self {
            graph,
            owner_authority,
            events,
        }
    }

    pub fn mutate(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: WorkProductMutationRequestV1,
        current_revisions: &WorkProductRevisionPinsV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        if &request.mutation_identity().revisions != current_revisions {
            return Err(WorkProductApplicationErrorV1::RevisionConflict);
        }
        match request {
            WorkProductMutationRequestV1::Create(request) => self.create(context, binding, request),
            WorkProductMutationRequestV1::AddTask(request) => {
                self.add_task(context, binding, *request)
            }
            WorkProductMutationRequestV1::CreateTask(request) => {
                self.create_task(context, binding, *request)
            }
            WorkProductMutationRequestV1::DecideProposal(request) => {
                self.decide_proposal(context, binding, request)
            }
            WorkProductMutationRequestV1::DecideRelationReplan(request) => {
                self.decide_relation_replan(context, binding, request)
            }
            WorkProductMutationRequestV1::ApplyRelationReplan(request) => {
                self.apply_relation_replan(context, binding, request)
            }
            WorkProductMutationRequestV1::AcceptTask(request) => {
                self.accept_task(context, binding, request)
            }
            WorkProductMutationRequestV1::AdmitExecution(request) => {
                self.admit_execution(context, binding, request)
            }
            WorkProductMutationRequestV1::LinkAcceptedAttempt(request) => {
                self.link_accepted_attempt(context, binding, request)
            }
            WorkProductMutationRequestV1::RecordHandoff(request) => {
                self.record_handoff(context, binding, request)
            }
        }
    }

    /// Prepare an exact mutation command from the current verified Work head.
    /// A later submit still performs normal graph-version and revision CAS, so
    /// state that changes between prepare and submit is rejected as stale.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_mutation(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: PrepareWorkProductMutationRequestV1,
        command_id: WorkCommandId,
        occurred_at: UtcMicros,
        revisions: WorkProductRevisionPinsV1,
    ) -> Result<WorkProductMutationRequestV1, WorkProductApplicationErrorV1> {
        authorize_and_admit(context, binding, occurred_at)?;
        request
            .selection
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let authorized_scope = self
            .owner_authority
            .authorize_scope(context, &request.selection, occurred_at)
            .map_err(map_owner_error)?;
        if authorized_scope.selection() != &request.selection {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
        let port_context =
            WorkProductPortContextV1::from_request(context, authorized_scope, occurred_at);
        let read_request = WorkGraphReadRequestV1::current(request.selection.clone(), occurred_at);
        let expected_authority = match self.graph.read_graph(&port_context, &read_request) {
            Ok(read) => {
                super::read::validate_result(
                    &read_request,
                    port_context.authorized_scope(),
                    &read,
                )?;
                // Reads answer over the covered slice and disclose the rest.
                // A mutation cannot: the head it would pin is the slice's
                // head, not the journal's, so the change would be formed
                // against a graph that is not current. Refused by name, with
                // the selection remedy, rather than left to surface later as a
                // version conflict that blames the wrong thing.
                if read.selection_coverage().is_partial() {
                    return Err(WorkProductApplicationErrorV1::SelectionCoverageIncomplete);
                }
                let WorkGraphReadV1::Current { snapshot, .. } = read else {
                    return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
                };
                WorkProductExpectedAuthorityV1::Verified {
                    verified_version: snapshot.verified_version().clone(),
                }
            }
            Err(super::WorkGraphReadPortErrorV1::NotFoundOrNotAuthorized)
                if matches!(&request.change, WorkProductChangeDraftV1::CreateTask { .. }) =>
            {
                let empty_request = WorkGraphReadRequestV1::forensic(
                    request.selection.clone(),
                    UtcMicros(i64::MIN),
                    occurred_at,
                    occurred_at,
                )?;
                let empty_read = self.graph.read_graph(&port_context, &empty_request)?;
                super::read::validate_result(
                    &empty_request,
                    port_context.authorized_scope(),
                    &empty_read,
                )?;
                // An empty covered slice is not the same fact as an empty
                // journal. Under partial coverage a graph exists outside this
                // selection, and creating a second root over it would append a
                // `Created` event to a journal that already has one.
                if empty_read.selection_coverage().is_partial() {
                    return Err(WorkProductApplicationErrorV1::SelectionCoverageIncomplete);
                }
                let WorkGraphReadV1::Forensic { timeline, .. } = empty_read else {
                    return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
                };
                if !timeline.entries().is_empty() {
                    return Err(WorkProductApplicationErrorV1::NotFoundOrNotAuthorized);
                }
                WorkProductExpectedAuthorityV1::NoPriorGraph
            }
            Err(error) => return Err(error.into()),
        };
        let expected_graph_version = match &expected_authority {
            WorkProductExpectedAuthorityV1::Verified { verified_version } => {
                Some(verified_version.graph_version())
            }
            WorkProductExpectedAuthorityV1::NoPriorGraph => None,
        };
        let mut mutation = WorkProductMutationIdentityV1 {
            expected_authority,
            command_id,
            causation_event_id: request.causation_event_id,
            evidence: request.evidence,
            occurred_at,
            revisions,
        };
        canonicalize_mutation_evidence(&mut mutation)?;
        let selection = request.selection;
        Ok(match request.change {
            WorkProductChangeDraftV1::AddTask { item } => {
                WorkProductMutationRequestV1::AddTask(Box::new(AddWorkTaskRequestV1 {
                    selection,
                    item: *item,
                    mutation,
                }))
            }
            WorkProductChangeDraftV1::CreateTask {
                initiative,
                plan,
                milestone,
                item,
            } => WorkProductMutationRequestV1::CreateTask(Box::new(CreateWorkTaskRequestV1 {
                selection,
                initiative,
                plan,
                milestone,
                item: *item,
                mutation,
            })),
            WorkProductChangeDraftV1::DecideProposal {
                proposal,
                disposition,
            } => WorkProductMutationRequestV1::DecideProposal(DecideWorkProposalRequestV1 {
                selection,
                proposal,
                disposition,
                mutation,
            }),
            WorkProductChangeDraftV1::DecideRelationReplan {
                proposal,
                disposition,
            } => WorkProductMutationRequestV1::DecideRelationReplan(
                DecideWorkRelationReplanRequestV1 {
                    selection,
                    proposal,
                    disposition,
                    mutation,
                },
            ),
            WorkProductChangeDraftV1::ApplyRelationReplan { proposal_id } => {
                WorkProductMutationRequestV1::ApplyRelationReplan(
                    ApplyWorkRelationReplanRequestV1 {
                        selection,
                        proposal_id,
                        mutation,
                    },
                )
            }
            WorkProductChangeDraftV1::AcceptTask {
                task_id,
                evidence_by_criterion,
            } => WorkProductMutationRequestV1::AcceptTask(AcceptWorkTaskRequestV1 {
                selection,
                task_id,
                evidence_by_criterion,
                mutation,
            }),
            WorkProductChangeDraftV1::AdmitExecution { task_id } => {
                WorkProductMutationRequestV1::AdmitExecution(AdmitWorkExecutionRequestV1 {
                    selection,
                    task_id,
                    based_on_version: expected_graph_version
                        .ok_or(WorkProductApplicationErrorV1::InvalidRequest)?,
                    mutation,
                })
            }
            WorkProductChangeDraftV1::LinkAcceptedAttempt { task_id, identity } => {
                WorkProductMutationRequestV1::LinkAcceptedAttempt(
                    LinkAcceptedWorkAttemptRequestV1 {
                        selection,
                        task_id,
                        based_on_version: expected_graph_version
                            .ok_or(WorkProductApplicationErrorV1::InvalidRequest)?,
                        identity,
                        mutation,
                    },
                )
            }
            WorkProductChangeDraftV1::RecordHandoff { handoff } => {
                WorkProductMutationRequestV1::RecordHandoff(RecordWorkHandoffRequestV1 {
                    selection,
                    handoff,
                    mutation,
                })
            }
        })
    }

    pub fn create(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: CreateWorkProductRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        self.commit_create(
            context,
            binding,
            request.selection,
            request.mutation,
            request.initial_graph,
        )
    }

    pub fn decide_proposal(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: DecideWorkProposalRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let change = if request.disposition == WorkProposalDispositionV1::Accepted {
            WorkGraphChangeV1::ProposalAccepted {
                proposal: request.proposal,
                accepted_at: request.mutation.occurred_at,
            }
        } else {
            WorkGraphChangeV1::ProposalDecided {
                proposal: request.proposal,
                disposition: request.disposition,
                decided_at: request.mutation.occurred_at,
            }
        };
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            change,
        )
    }

    pub fn add_task(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: AddWorkTaskRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            WorkGraphChangeV1::TaskAdded {
                item: Box::new(request.item),
            },
        )
    }

    /// Creates one exact task and its declared hierarchy. The first task owns
    /// graph bootstrap; later tasks use the same version-checked event path
    /// and may reuse byte-identical containers. No daemon-side default
    /// hierarchy or separate bootstrap authority exists.
    pub fn create_task(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: CreateWorkTaskRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let CreateWorkTaskRequestV1 {
            selection,
            initiative,
            plan,
            milestone,
            item,
            mutation,
        } = request;
        match &mutation.expected_authority {
            WorkProductExpectedAuthorityV1::NoPriorGraph => {
                let graph = WorkProductGraphV1::new(
                    WorkGraphVersionV1::initial(),
                    vec![initiative],
                    vec![plan],
                    vec![milestone],
                    vec![item],
                )
                .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
                self.commit_create(context, binding, selection, mutation, graph)
            }
            WorkProductExpectedAuthorityV1::Verified { .. } => self.commit_change(
                context,
                binding,
                selection,
                mutation,
                WorkGraphChangeV1::TaskCreated {
                    initiative,
                    plan,
                    milestone,
                    item: Box::new(item),
                },
            ),
        }
    }

    pub fn decide_relation_replan(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: DecideWorkRelationReplanRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let decided_at = request.mutation.occurred_at;
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            WorkGraphChangeV1::RelationReplanDecided {
                proposal: request.proposal,
                disposition: request.disposition,
                decided_at,
            },
        )
    }

    pub fn apply_relation_replan(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: ApplyWorkRelationReplanRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let change = WorkGraphChangeV1::TaskRelationsReplanned {
            proposal_id: request.proposal_id,
            applied_at: request.mutation.occurred_at,
        };
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            change,
        )
    }

    pub fn accept_task(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: AcceptWorkTaskRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let change = WorkGraphChangeV1::TaskAccepted {
            task_id: request.task_id,
            evidence_by_criterion: request.evidence_by_criterion,
            accepted_at: request.mutation.occurred_at,
        };
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            change,
        )
    }

    pub fn admit_execution(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: AdmitWorkExecutionRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let admitted_at = request.mutation.occurred_at;
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            WorkGraphChangeV1::ExecutionAdmitted {
                task_id: request.task_id,
                based_on_version: request.based_on_version,
                admitted_at,
            },
        )
    }

    /// Links one exact admitted attempt identity. Terminal evidence remains
    /// owned by the attempt and task evidence is linked independently.
    pub fn link_accepted_attempt(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: LinkAcceptedWorkAttemptRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let linked_at = request.mutation.occurred_at;
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: request.task_id,
                based_on_version: request.based_on_version,
                identity: request.identity,
                linked_at,
            },
        )
    }

    pub fn record_handoff(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: RecordWorkHandoffRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            WorkGraphChangeV1::HandoffRecorded {
                handoff: request.handoff,
            },
        )
    }

    fn commit_create(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        selection: WorkProductSelectionScopeV1,
        mutation: WorkProductMutationIdentityV1,
        graph: WorkProductGraphV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let payload = WorkProductEventPayloadV1::Created { graph };
        let (port_context, mutation, digest) =
            self.prepare(context, binding, &selection, mutation, &payload)?;
        let WorkProductEventPayloadV1::Created { graph } = &payload else {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        };
        if !matches!(
            &mutation.expected_authority,
            WorkProductExpectedAuthorityV1::NoPriorGraph
        ) || !mutation.evidence.is_empty()
            || graph.version() != WorkGraphVersionV1::initial()
            || graph.validate().is_err()
        {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        if let Some(commit) = self.replay(&port_context, &mutation, &payload, &digest)? {
            return mutation_receipt(commit, true);
        }
        let draft = event_draft(
            context,
            port_context.authorized_scope(),
            &selection,
            &mutation,
            digest,
            WorkGraphVersionV1::initial(),
            payload,
        )?;
        self.append_atomically(&port_context, draft)
    }

    fn commit_change(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        selection: WorkProductSelectionScopeV1,
        mutation: WorkProductMutationIdentityV1,
        change: WorkGraphChangeV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let payload = WorkProductEventPayloadV1::Changed {
            change: Box::new(change.clone()),
        };
        let (port_context, mutation, digest) =
            self.prepare(context, binding, &selection, mutation, &payload)?;
        let WorkProductExpectedAuthorityV1::Verified {
            verified_version: expected_verified_version,
        } = &mutation.expected_authority
        else {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        };
        let expected_graph_version = expected_verified_version.graph_version();
        validate_change_request(&change, expected_graph_version, mutation.occurred_at)?;
        if let Some(commit) = self.replay(&port_context, &mutation, &payload, &digest)? {
            return mutation_receipt(commit, true);
        }

        let read_request = WorkGraphReadRequestV1::current(selection.clone(), mutation.occurred_at);
        let read = self.graph.read_graph(&port_context, &read_request)?;
        super::read::validate_result(&read_request, port_context.authorized_scope(), &read)?;
        // The same rule the prepare enforces: a covered slice has a head, but
        // not the journal's head, so a submit against it is refused by name
        // instead of failing its compare-and-swap for the wrong reason.
        if read.selection_coverage().is_partial() {
            return Err(WorkProductApplicationErrorV1::SelectionCoverageIncomplete);
        }
        let WorkGraphReadV1::Current { snapshot, .. } = read else {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        };
        if snapshot.verified_version() != expected_verified_version {
            return Err(WorkProductApplicationErrorV1::VersionConflict);
        }
        let result_graph = snapshot
            .graph()
            .clone()
            .apply(change.clone())
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let draft = event_draft(
            context,
            port_context.authorized_scope(),
            &selection,
            &mutation,
            digest,
            result_graph.version(),
            payload,
        )?;
        self.append_atomically(&port_context, draft)
    }

    fn prepare(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        selection: &WorkProductSelectionScopeV1,
        mut mutation: WorkProductMutationIdentityV1,
        payload: &WorkProductEventPayloadV1,
    ) -> Result<
        (
            WorkProductPortContextV1,
            WorkProductMutationIdentityV1,
            ManifestDigest,
        ),
        WorkProductApplicationErrorV1,
    > {
        authorize_and_admit(context, binding, mutation.occurred_at)?;
        selection
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let authorized_scope = self
            .owner_authority
            .authorize_scope(context, selection, mutation.occurred_at)
            .map_err(map_owner_error)?;
        if authorized_scope.selection() != selection {
            return Err(WorkProductApplicationErrorV1::EventAuthorityUnavailable);
        }
        canonicalize_mutation_evidence(&mut mutation)?;
        let digest = canonical_work_product_mutation_digest(
            context.actor(),
            &authorized_scope,
            selection,
            &mutation,
            payload,
        )?;
        Ok((
            WorkProductPortContextV1::from_request(context, authorized_scope, mutation.occurred_at),
            mutation,
            digest,
        ))
    }

    fn replay(
        &self,
        port_context: &WorkProductPortContextV1,
        mutation: &WorkProductMutationIdentityV1,
        payload: &WorkProductEventPayloadV1,
        digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventCommitV1>, WorkProductApplicationErrorV1> {
        let commit = self
            .events
            .replay(port_context, &mutation.command_id, digest)
            .map_err(map_event_error)?;
        if let Some(commit) = commit {
            commit.validate().map_err(map_event_error)?;
            validate_replayed_event(
                commit.event(),
                port_context,
                mutation,
                payload,
                digest,
                &selected_relations(port_context.authorized_scope().selection()),
            )?;
            return Ok(Some(commit));
        }
        Ok(None)
    }

    fn append_atomically(
        &self,
        port_context: &WorkProductPortContextV1,
        draft: WorkProductEventDraftV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let (commit, replayed) = self
            .events
            .append_atomically(port_context, &draft)
            .map_err(map_event_error)?
            .into_parts();
        commit.validate().map_err(map_event_error)?;
        validate_appended_event(commit.event(), &draft)?;
        mutation_receipt(commit, replayed)
    }
}

fn mutation_receipt(
    commit: WorkProductEventCommitV1,
    replayed: bool,
) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
    commit.validate().map_err(map_event_error)?;
    let (event, verified_graph_version) = commit.into_parts();
    Ok(WorkProductMutationReceiptV1 {
        event,
        verified_graph_version,
        replayed,
    })
}

fn canonical_work_product_mutation_digest(
    actor: &ActorId,
    authorized_scope: &AuthorizedWorkProductScopeV1,
    selection: &WorkProductSelectionScopeV1,
    mutation: &WorkProductMutationIdentityV1,
    payload: &WorkProductEventPayloadV1,
) -> Result<ManifestDigest, WorkProductApplicationErrorV1> {
    canonical_sha256(&(
        WORK_PRODUCT_MUTATION_DIGEST_DOMAIN,
        actor,
        authorized_scope,
        selection,
        &mutation.expected_authority,
        &mutation.command_id,
        &mutation.causation_event_id,
        &mutation.evidence,
        mutation.occurred_at,
        &mutation.revisions,
        payload,
    ))
    .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)
}

fn canonicalize_mutation_evidence(
    mutation: &mut WorkProductMutationIdentityV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if mutation.evidence.len() > MAX_WORK_PRODUCT_EVENT_EVIDENCE {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    mutation.evidence.sort();
    let source_watermark = mutation_source_watermark(&mutation.expected_authority)?;
    if mutation.evidence.windows(2).any(|pair| pair[0] == pair[1])
        || mutation.evidence.iter().any(|evidence| {
            !source_watermark
                .components()
                .contains_key(&evidence.source_store_id)
        })
    {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    Ok(())
}

fn mutation_source_watermark(
    authority: &WorkProductExpectedAuthorityV1,
) -> Result<WorkProductSourceWatermarkV1, WorkProductApplicationErrorV1> {
    match authority {
        WorkProductExpectedAuthorityV1::NoPriorGraph => {
            WorkProductSourceWatermarkV1::new(BTreeMap::new())
                .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)
        }
        WorkProductExpectedAuthorityV1::Verified { verified_version } => {
            Ok(verified_version.source_watermark().clone())
        }
    }
}

fn mutation_expected_graph_version(
    authority: &WorkProductExpectedAuthorityV1,
) -> Option<WorkGraphVersionV1> {
    match authority {
        WorkProductExpectedAuthorityV1::NoPriorGraph => None,
        WorkProductExpectedAuthorityV1::Verified { verified_version } => {
            Some(verified_version.graph_version())
        }
    }
}

fn validate_change_request(
    change: &WorkGraphChangeV1,
    expected_graph_version: WorkGraphVersionV1,
    occurred_at: UtcMicros,
) -> Result<(), WorkProductApplicationErrorV1> {
    if let WorkGraphChangeV1::ExecutionAdmitted {
        based_on_version,
        admitted_at,
        ..
    } = change
        && (*admitted_at != occurred_at || *based_on_version != expected_graph_version)
    {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    if let WorkGraphChangeV1::AcceptedAttemptLinked {
        task_id,
        based_on_version,
        identity,
        linked_at,
    } = change
        && (identity.task_id() != task_id
            || *linked_at != occurred_at
            || *based_on_version != expected_graph_version)
    {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    Ok(())
}

fn event_draft(
    context: &RequestContext,
    authorized_scope: &AuthorizedWorkProductScopeV1,
    selection: &WorkProductSelectionScopeV1,
    mutation: &WorkProductMutationIdentityV1,
    canonical_input_digest: ManifestDigest,
    result_graph_version: WorkGraphVersionV1,
    payload: WorkProductEventPayloadV1,
) -> Result<WorkProductEventDraftV1, WorkProductApplicationErrorV1> {
    Ok(WorkProductEventDraftV1 {
        actor_id: context.actor().clone(),
        owner_scope: WorkProductProfileScopeV1 {
            brain_id: authorized_scope.owner_brain_id().clone(),
            profile_id: authorized_scope.owner_profile_id().clone(),
        },
        authorized_relation_scopes: selected_relations(selection),
        expected_graph_version: mutation_expected_graph_version(&mutation.expected_authority),
        result_graph_version,
        command_id: mutation.command_id.clone(),
        canonical_input_digest,
        causation_event_id: mutation.causation_event_id.clone(),
        evidence: mutation.evidence.clone(),
        source_watermark: mutation_source_watermark(&mutation.expected_authority)?,
        occurred_at: mutation.occurred_at,
        policy_revision_id: mutation.revisions.policy_revision_id.clone(),
        configuration_revision_id: mutation.revisions.configuration_revision_id.clone(),
        catalog_generation_id: mutation.revisions.catalog_generation_id.clone(),
        payload,
    })
}

fn selected_relations(
    selection: &WorkProductSelectionScopeV1,
) -> Vec<tracedecay_domain::WorkProductAuthorizedRelationScopeV1> {
    selection
        .relation_scopes()
        .map_or_else(Vec::new, |relations| relations.iter().cloned().collect())
}

fn authorize_and_admit(
    context: &RequestContext,
    binding: &WorkProductBindingV1,
    observed_at: UtcMicros,
) -> Result<(), WorkProductApplicationErrorV1> {
    if !context.allows(binding.capability_id(), binding.use_case_id()) {
        return Err(WorkProductApplicationErrorV1::NotAuthorized);
    }
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(WorkProductApplicationErrorV1::Cancelled),
        RequestAdmission::TimedOut => Err(WorkProductApplicationErrorV1::TimedOut),
    }
}

fn map_owner_error(error: WorkProductOwnerAuthorizationErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkProductOwnerAuthorizationErrorV1::NotAuthorized => {
            WorkProductApplicationErrorV1::NotAuthorized
        }
        WorkProductOwnerAuthorizationErrorV1::Unavailable => {
            WorkProductApplicationErrorV1::EventAuthorityUnavailable
        }
    }
}

fn map_event_error(error: WorkProductEventPortErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkProductEventPortErrorV1::NotFoundOrNotAuthorized => {
            WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
        }
        WorkProductEventPortErrorV1::VersionConflict => {
            WorkProductApplicationErrorV1::VersionConflict
        }
        WorkProductEventPortErrorV1::IdempotencyConflict => {
            WorkProductApplicationErrorV1::IdempotencyConflict
        }
        WorkProductEventPortErrorV1::Unavailable => {
            WorkProductApplicationErrorV1::EventAuthorityUnavailable
        }
        WorkProductEventPortErrorV1::Cancelled => WorkProductApplicationErrorV1::Cancelled,
        WorkProductEventPortErrorV1::TimedOut => WorkProductApplicationErrorV1::TimedOut,
    }
}

fn validate_replayed_event(
    event: &WorkProductEventV1,
    context: &WorkProductPortContextV1,
    mutation: &WorkProductMutationIdentityV1,
    payload: &WorkProductEventPayloadV1,
    canonical_input_digest: &ManifestDigest,
    authorized_relation_scopes: &[tracedecay_domain::WorkProductAuthorizedRelationScopeV1],
) -> Result<(), WorkProductApplicationErrorV1> {
    let expected_result_version = match payload {
        WorkProductEventPayloadV1::Created { .. } => WorkGraphVersionV1::initial(),
        WorkProductEventPayloadV1::Changed { .. } => {
            mutation_expected_graph_version(&mutation.expected_authority)
                .and_then(|version| version.next().ok())
                .ok_or(WorkProductApplicationErrorV1::IdempotencyConflict)?
        }
    };
    let expected_graph_version = mutation_expected_graph_version(&mutation.expected_authority);
    let source_watermark = mutation_source_watermark(&mutation.expected_authority)
        .map_err(|_| WorkProductApplicationErrorV1::IdempotencyConflict)?;
    if event.actor_id() != context.actor()
        || &event.owner_scope().brain_id != context.authorized_scope().owner_brain_id()
        || &event.owner_scope().profile_id != context.authorized_scope().owner_profile_id()
        || event.authorized_relation_scopes() != authorized_relation_scopes
        || event.expected_graph_version() != expected_graph_version
        || event.result_graph_version() != expected_result_version
        || event.command_id() != &mutation.command_id
        || event.canonical_input_digest() != canonical_input_digest
        || event.causation_event_id() != mutation.causation_event_id.as_ref()
        || event.evidence() != mutation.evidence
        || event.source_watermark() != &source_watermark
        || event.occurred_at() != mutation.occurred_at
        || event.policy_revision_id() != &mutation.revisions.policy_revision_id
        || event.configuration_revision_id() != &mutation.revisions.configuration_revision_id
        || event.catalog_generation_id() != &mutation.revisions.catalog_generation_id
        || event.payload() != payload
    {
        return Err(WorkProductApplicationErrorV1::IdempotencyConflict);
    }
    Ok(())
}

fn validate_appended_event(
    event: &WorkProductEventV1,
    draft: &WorkProductEventDraftV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if event.actor_id() != &draft.actor_id
        || event.owner_scope() != &draft.owner_scope
        || event.authorized_relation_scopes() != draft.authorized_relation_scopes
        || event.expected_graph_version() != draft.expected_graph_version
        || event.result_graph_version() != draft.result_graph_version
        || event.command_id() != &draft.command_id
        || event.canonical_input_digest() != &draft.canonical_input_digest
        || event.causation_event_id() != draft.causation_event_id.as_ref()
        || event.evidence() != draft.evidence
        || event.source_watermark() != &draft.source_watermark
        || event.occurred_at() != draft.occurred_at
        || event.policy_revision_id() != &draft.policy_revision_id
        || event.configuration_revision_id() != &draft.configuration_revision_id
        || event.catalog_generation_id() != &draft.catalog_generation_id
        || event.payload() != &draft.payload
    {
        return Err(WorkProductApplicationErrorV1::EventAuthorityUnavailable);
    }
    Ok(())
}
