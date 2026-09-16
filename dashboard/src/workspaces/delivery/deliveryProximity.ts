import type {
  DeliveryAttentionEvidenceV1,
  DeliveryAttentionItemV1,
  DeliveryAttentionSourceV1,
  DeliveryInboxCoverageV1,
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
} from '../../contracts/generated.ts';
import type {
  FeedbackProximityEncounterV1,
  FeedbackProximityReadResultV1,
  FeedbackProximityRelationV1,
} from '../../contracts/index.ts';

/** The three Delivery attention sources a proximity encounter can settle,
 * mirroring the daemon's `apply_proximity_attention` in
 * `tracedecay-application/src/delivery.rs`. */
const PROXIMITY_ATTENTION_SOURCES: ReadonlySet<DeliveryAttentionSourceV1> = new Set([
  'overlapping_edit',
  'confirmed_conflict',
  'divergent_shared_implementation',
]);

function proximityAttentionSource(
  relationKind: FeedbackProximityRelationV1['relation_kind'],
): DeliveryAttentionSourceV1 | null {
  switch (relationKind) {
    case 'overlapping_edit':
      return 'overlapping_edit';
    case 'confirmed_conflict':
      return 'confirmed_conflict';
    case 'shared_code_candidate':
      return 'divergent_shared_implementation';
    // Loom's neighborhood hint has no Delivery attention counterpart.
    case 'code_neighborhood_candidate':
      return null;
    default: {
      const unhandled: never = relationKind;
      return unhandled;
    }
  }
}

function proximityEvidence(encounter: FeedbackProximityEncounterV1): DeliveryAttentionEvidenceV1 {
  return {
    kind: 'proximity_encounter',
    encounter_id: encounter.encounter_id,
    relation_kind: encounter.relation.relation_kind,
  };
}

function encounterMatchesHead(
  encounter: FeedbackProximityEncounterV1,
  indexedHeadCommitId: string,
): boolean {
  return encounter.participants.some(
    (participant) => participant.head_revision === indexedHeadCommitId,
  );
}

/** The first encounter admitted per source: encounters arrive in the page's
 * observed order, so the earliest relation for a source wins the same way
 * the daemon's aggregation is stable. */
function matchedEncountersBySource(
  encounters: readonly FeedbackProximityEncounterV1[],
  indexedHeadCommitId: string,
): ReadonlyMap<DeliveryAttentionSourceV1, FeedbackProximityEncounterV1> {
  const matched = new Map<DeliveryAttentionSourceV1, FeedbackProximityEncounterV1>();
  for (const encounter of encounters) {
    const source = proximityAttentionSource(encounter.relation.relation_kind);
    if (source === null || matched.has(source)) continue;
    if (encounterMatchesHead(encounter, indexedHeadCommitId)) {
      matched.set(source, encounter);
    }
  }
  return matched;
}

function withProximityAttention(
  item: DeliveryAttentionItemV1,
  apply: (item: DeliveryAttentionItemV1) => DeliveryAttentionItemV1,
): DeliveryAttentionItemV1 {
  return PROXIMITY_ATTENTION_SOURCES.has(item.source) ? apply(item) : item;
}

function applyEncountersToPullRequest(
  pullRequest: DeliveryInboxPullRequestV1,
  encounters: readonly FeedbackProximityEncounterV1[],
  observedAtMicros: number,
  coverage: DeliveryInboxCoverageV1,
): DeliveryInboxPullRequestV1 {
  const matched = matchedEncountersBySource(encounters, pullRequest.indexed_head_commit_id);
  return {
    ...pullRequest,
    attention: pullRequest.attention.map((item) =>
      withProximityAttention(item, (item) => {
        const encounter = matched.get(item.source);
        return encounter === undefined
          ? {
              ...item,
              state: 'clear',
              coverage,
              evidence: [],
              observed_at_micros: observedAtMicros,
            }
          : {
              ...item,
              state: 'active',
              coverage,
              evidence: [proximityEvidence(encounter)],
              observed_at_micros: encounter.observed_at,
            };
      }),
    ),
  };
}

/** Preserve the proximity read's typed coverage on Delivery attention items.
 * `partial`/`stale` mean the authority answered with omissions — never collapse
 * those into `complete` or a Clear finding will read as fully measured. */
function coverageForProximityState(
  state: Extract<
    FeedbackProximityReadResultV1,
    { state: 'complete' | 'complete_zero' | 'partial' | 'stale' }
  >['state'],
): DeliveryInboxCoverageV1 {
  switch (state) {
    case 'complete':
    case 'complete_zero':
      return 'complete';
    case 'partial':
      return 'partial';
    case 'stale':
      return 'stale';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

function applyUnavailableToPullRequest(
  pullRequest: DeliveryInboxPullRequestV1,
  observedAtMicros: number,
): DeliveryInboxPullRequestV1 {
  return {
    ...pullRequest,
    attention: pullRequest.attention.map((item) =>
      withProximityAttention(item, (item) => ({
        ...item,
        state: 'unavailable',
        coverage: 'unavailable',
        evidence: [],
        observed_at_micros: observedAtMicros,
      })),
    ),
  };
}

/**
 * Joins the separately-read `/api/feedback/proximity` page into the
 * `/api/delivery/inbox` payload's `overlapping_edit`, `confirmed_conflict`
 * and `divergent_shared_implementation` attention sources, the same join
 * `useLoomProximity` performs for Loom's weave. The dashboard's inbox HTTP
 * handler leaves these sources `unsupported` on purpose (see
 * `delivery_api.rs::inbox`); this is where they become live.
 *
 * `proximity === undefined` means the read has not resolved yet (or was not
 * requested), so server-served attention is left untouched rather than
 * flashed to `unavailable`.
 */
export function applyProximityAttention(
  inbox: DeliveryInboxV1,
  proximity: FeedbackProximityReadResultV1 | undefined,
): DeliveryInboxV1 {
  if (proximity === undefined) return inbox;

  switch (proximity.state) {
    case 'denied':
    case 'unavailable':
      return {
        ...inbox,
        pull_requests: inbox.pull_requests.map((pullRequest) =>
          applyUnavailableToPullRequest(pullRequest, proximity.observed_at),
        ),
      };
    case 'complete':
    case 'complete_zero':
    case 'partial':
    case 'stale':
      return {
        ...inbox,
        pull_requests: inbox.pull_requests.map((pullRequest) =>
          applyEncountersToPullRequest(
            pullRequest,
            proximity.page.encounters,
            proximity.page.observed_at,
            coverageForProximityState(proximity.state),
          ),
        ),
      };
    default: {
      const unhandled: never = proximity;
      return unhandled;
    }
  }
}
