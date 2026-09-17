/**
 * Delivery inbox readings that an operator can act on.
 *
 * The wire is honest about every attention source and about the fact that a
 * pull request does not name a symbol or a base revision. This module is the
 * experience of that honesty: controls complete a next step, or they are not
 * offered. A link that opens a Code view the dashboard then blocks is not a
 * next step.
 */
import type {
  DeliveryAttentionEvidenceV1,
  DeliveryAttentionItemV1,
  DeliveryAttentionStateV1,
  DeliveryGitHubReadOperationV1,
  DeliveryInboxCoverageV1,
  DeliveryProximityRelationV1,
  DeliveryProviderStateV1,
} from '../../contracts/generated.ts';

export interface CodeNextStep {
  readonly href: string;
  readonly label: string;
  readonly detail: string;
}

export function attentionNeedsOperator(item: DeliveryAttentionItemV1): boolean {
  return item.state === 'active' || item.state === 'denied';
}

/** Active and denied lead. Clear and unreadable sources stay visible, but
 * they are not the reason the operator opened the inbox. */
export function rankAttention(
  items: readonly DeliveryAttentionItemV1[],
): DeliveryAttentionItemV1[] {
  return items
    .map((item, index) => ({ item, index }))
    .sort((left, right) => {
      const byState = attentionRank(left.item.state) - attentionRank(right.item.state);
      return byState === 0 ? left.index - right.index : byState;
    })
    .map((entry) => entry.item);
}

function attentionRank(state: DeliveryAttentionStateV1): number {
  switch (state) {
    case 'active':
      return 0;
    case 'denied':
      return 1;
    case 'clear':
      return 2;
    case 'unavailable':
      return 3;
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

/**
 * An attention filter is a request for work, not for "the source exists on
 * the row". Every admitted pull request carries every source, including ones
 * that are unsupported, so presence-matching shows the whole inbox.
 */
export function pullRequestHasOperatorAttention(
  attention: readonly DeliveryAttentionItemV1[],
  source: DeliveryAttentionItemV1['source'],
): boolean {
  return attention.some((item) => item.source === source && attentionNeedsOperator(item));
}

export function settledAttentionReason(
  state: DeliveryAttentionStateV1,
  coverage: DeliveryInboxCoverageV1,
): string {
  if (state === 'denied' || coverage === 'denied') return 'Access was denied.';
  switch (coverage) {
    case 'unsupported':
      return 'Not available for this pull request.';
    case 'unavailable':
      return 'Could not be read.';
    case 'stale':
      return state === 'clear' ? 'Clear, but the reading may be stale.' : 'Reading may be stale.';
    case 'partial':
      return state === 'clear' ? 'Clear on a partial reading.' : 'Partial reading.';
    case 'complete':
      return state === 'clear' ? 'Clear.' : 'Recorded.';
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

export interface EvidenceReading {
  readonly headline: string;
  readonly detail: string;
  /** Opaque wire identity. Secondary, never the sentence the operator reads. */
  readonly reference: string;
}

export function evidenceReading(evidence: DeliveryAttentionEvidenceV1): EvidenceReading {
  switch (evidence.kind) {
    case 'ci_failure':
      return {
        headline: 'A check failed on this indexed head.',
        detail: 'The failure is recorded against the commit this pull request was admitted for.',
        reference: evidence.failure_anchor,
      };
    case 'review_comment':
      return {
        headline: `Review comment on ${evidence.path}.`,
        detail: 'Open the provider thread for the comment body. This inbox does not replay it.',
        reference: evidence.comment_id,
      };
    case 'proximity_encounter':
      return {
        headline: proximityRelationLabel(evidence.relation),
        detail: 'Select a symbol in Code to inspect this encounter. This pull request does not name one.',
        reference: evidence.encounter_id,
      };
    case 'provider_operation':
      return {
        headline: `${operationLabel(evidence.operation)} was read.`,
        detail: `Fetched ${formatFetchedAt(evidence.fetched_at_micros)}.`,
        reference: evidence.operation,
      };
    case 'indexed_generation':
      return {
        headline: 'This attention is bound to one sealed generation.',
        detail: 'Queries against a later generation are a different reading.',
        reference: evidence.generation,
      };
    default: {
      const unhandled: never = evidence;
      return unhandled;
    }
  }
}

export function providerStateLabel(state: DeliveryProviderStateV1): string {
  switch (state) {
    case 'ready':
      return 'Ready';
    case 'partial':
      return 'Partial';
    case 'stale':
      return 'Stale';
    case 'rate_limited':
      return 'Rate limited';
    case 'failed':
      return 'Failed';
    case 'denied':
      return 'Denied';
    case 'not_published':
      return 'Not published';
    case 'not_configured':
      return 'Not configured';
    case 'unavailable':
      return 'Unavailable';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

/**
 * Next steps from facts the inbox already has.
 *
 * `codePath` is the scoped Code workspace (`/code` or `/code?scope=…`).
 * Compare carries the indexed head so the operator only names the base.
 * Shared Code is not linked: the row has no symbol, and `?view=shared-code`
 * without one is a blocked page.
 */
export function codeNextSteps(
  codePath: string,
  pullRequest: {
    readonly branch_ref: string;
    readonly indexed_head_commit_id: string;
  },
): { readonly compare: CodeNextStep | null; readonly selectSymbol: CodeNextStep } {
  const branch = branchNameFromRef(pullRequest.branch_ref);
  const revision = pullRequest.indexed_head_commit_id;
  const compare =
    branch === '' || revision === ''
      ? null
      : {
          href: appendWorkspaceSearch(codePath, compareHeadSearch(branch, revision)),
          label: 'Compare this head',
          detail: `Head is ${branch} at ${revision.slice(0, 12)}. Name the base revision next. Nothing is compared until both revisions are exact.`,
        };
  return {
    compare,
    selectSymbol: {
      href: codePath,
      label: 'Select a symbol in Code',
      detail:
        'Shared Code opens after a symbol is selected. This pull request does not name one, so this link does not open a blocked Shared Code view.',
    },
  };
}

export function compareHeadSearch(branch: string, revision: string): URLSearchParams {
  const params = new URLSearchParams();
  params.set('view', 'compare');
  params.set('head', branch);
  params.set('head_revision', revision);
  return params;
}

export function appendWorkspaceSearch(path: string, extra: URLSearchParams): string {
  const url = new URL(path, 'http://local.invalid');
  for (const [key, value] of extra) {
    url.searchParams.set(key, value);
  }
  const search = url.searchParams.toString();
  return search === '' ? url.pathname : `${url.pathname}?${search}`;
}

function branchNameFromRef(reference: string): string {
  return reference.startsWith('refs/heads/') ? reference.slice('refs/heads/'.length) : reference;
}

function proximityRelationLabel(relation: DeliveryProximityRelationV1): string {
  switch (relation) {
    case 'code_neighborhood_candidate':
      return 'Code neighborhood recorded.';
    case 'shared_code_candidate':
      return 'Shared-code candidate recorded.';
    case 'overlapping_edit':
      return 'Overlapping edit recorded.';
    case 'confirmed_conflict':
      return 'Confirmed conflict recorded.';
    default: {
      const unhandled: never = relation;
      return unhandled;
    }
  }
}

function operationLabel(operation: DeliveryGitHubReadOperationV1): string {
  switch (operation) {
    case 'pull_request':
      return 'Pull request';
    case 'review_comments':
      return 'Review comments';
    case 'review_threads':
      return 'Review threads';
    case 'reviews':
      return 'Reviews';
    default: {
      const unhandled: never = operation;
      return unhandled;
    }
  }
}

function formatFetchedAt(micros: number): string {
  return new Date(Math.floor(micros / 1_000)).toISOString();
}
