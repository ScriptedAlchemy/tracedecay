import {
  DeliveryReviewLifecycleV1Schema,
  type DeliveryCiCheckV1,
  type DeliveryGitHubOperationSnapshotV1,
  type DeliveryPullRequestV1,
  type DeliveryReviewItemV1,
  type DeliveryReviewLifecycleV1,
  type DeliveryReviewObservationV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { EvidenceGrade } from './evidence.ts';

/**
 * The exact review workspace over Delivery's provider projections: review
 * threads keyed by lifecycle and path, the check matrix, and the identity the
 * provider reported for the pull request. No control here posts, resolves,
 * re-runs or merges anything; the only outward pivots are the provider's own
 * `source_url` and the Code workspace's Compare view.
 */

/** `refs/heads/feature/x` → `feature/x`; Compare resolves branch names. */
export function branchName(branchRef: string): string {
  return branchRef.startsWith('refs/heads/') ? branchRef.slice('refs/heads/'.length) : branchRef;
}

/**
 * A Compare deep link with the head half prefilled from exact Delivery
 * identities. The base half is left for the reader because the provider
 * projection does not carry the base branch name, and Compare refuses to run
 * on an incomplete pair, so this is a why-to-code pivot that requires
 * selection, never a silently guessed comparison.
 */
export function compareHref(target: {
  readonly branch: string;
  readonly revision: string;
  readonly file: string | null;
}): string {
  const params = new URLSearchParams();
  params.set('view', 'compare');
  params.set('head', branchName(target.branch));
  params.set('head_revision', target.revision);
  if (target.file !== null && target.file !== '') params.set('compare_file', target.file);
  return `/code?${params.toString()}`;
}

export const REVIEW_LIFECYCLES = DeliveryReviewLifecycleV1Schema.options;

export interface ReviewThread {
  readonly id: string;
  readonly item: DeliveryReviewItemV1;
  readonly latest: DeliveryReviewObservationV1;
  readonly grade: EvidenceGrade;
  readonly compareHref: string;
}

export interface ReviewFile {
  readonly path: string;
  readonly threads: readonly ReviewThread[];
}

export interface ReviewLanes {
  readonly counts: Readonly<Record<DeliveryReviewLifecycleV1, number>>;
  readonly files: readonly ReviewFile[];
  readonly threads: readonly ReviewThread[];
  /** Items whose observations were all absent, never counted as resolved. */
  readonly unobserved: number;
}

export function latestObservation(
  item: DeliveryReviewItemV1,
): DeliveryReviewObservationV1 | null {
  let found: DeliveryReviewObservationV1 | null = null;
  for (const observation of item.observations) {
    if (found === null || observation.observed_at_micros > found.observed_at_micros) {
      found = observation;
    }
  }
  return found;
}

export function buildReviewLanes(
  items: readonly DeliveryReviewItemV1[],
  head: { readonly branch: string; readonly revision: string },
  filter: DeliveryReviewLifecycleV1 | null,
): ReviewLanes {
  const counts: Record<DeliveryReviewLifecycleV1, number> = {
    current: 0,
    outdated: 0,
    resolved: 0,
    edited: 0,
    deleted: 0,
  };
  let unobserved = 0;
  const threads: ReviewThread[] = [];
  for (const item of items) {
    const observation = latestObservation(item);
    if (observation === null) {
      unobserved += 1;
      continue;
    }
    counts[observation.lifecycle] += 1;
    if (filter !== null && observation.lifecycle !== filter) continue;
    threads.push({
      id: item.id,
      item,
      latest: observation,
      grade: observation.provider_outcome === 'stale' ? 'stale' : 'exact',
      compareHref: compareHref({ branch: head.branch, revision: head.revision, file: observation.path }),
    });
  }
  threads.sort(
    (left, right) =>
      left.latest.path.localeCompare(right.latest.path) ||
      (left.latest.line ?? 0) - (right.latest.line ?? 0) ||
      left.id.localeCompare(right.id),
  );
  const byPath = new Map<string, ReviewThread[]>();
  for (const thread of threads) {
    const list = byPath.get(thread.latest.path) ?? [];
    list.push(thread);
    byPath.set(thread.latest.path, list);
  }
  return {
    counts,
    threads,
    unobserved,
    files: [...byPath.entries()].map(([path, list]) => ({ path, threads: list })),
  };
}

export function lifecycleKind(lifecycle: DeliveryReviewLifecycleV1): DomainStateKind {
  switch (lifecycle) {
    case 'current':
      return 'partial';
    case 'outdated':
      return 'stale';
    case 'resolved':
      return 'ready';
    case 'edited':
      return 'partial';
    case 'deleted':
      return 'cancelled';
    default: {
      const unhandled: never = lifecycle;
      return unhandled;
    }
  }
}

export interface CheckRow {
  readonly check: DeliveryCiCheckV1;
  readonly status: DomainStateKind;
  readonly compareHref: string | null;
}

export function buildCheckRows(
  checks: readonly DeliveryCiCheckV1[],
  head: { readonly branch: string; readonly revision: string },
): readonly CheckRow[] {
  return [...checks]
    .sort(
      (left, right) =>
        left.workflow_path.localeCompare(right.workflow_path) ||
        left.label.localeCompare(right.label) ||
        left.id.localeCompare(right.id),
    )
    .map((check) => {
      const path = check.annotations[0]?.path ?? null;
      return {
        check,
        status: checkStatusKind(check),
        compareHref:
          path === null
            ? null
            : compareHref({ branch: head.branch, revision: head.revision, file: path }),
      };
    });
}

export function checkStatusKind(check: DeliveryCiCheckV1): DomainStateKind {
  if (check.check_conclusion === null) {
    switch (check.check_status) {
      case 'completed':
        return 'unknown';
      case 'failed':
        return 'error';
      case 'in_progress':
      case 'pending':
      case 'queued':
      case 'waiting':
        return 'loading';
      default: {
        const unhandled: never = check.check_status;
        return unhandled;
      }
    }
  }
  switch (check.check_conclusion) {
    case 'success':
      return 'ready';
    case 'failure':
      return 'error';
    case 'action_required':
      return 'partial';
    case 'cancelled':
      return 'cancelled';
    case 'timed_out':
      return 'timed_out';
    case 'neutral':
    case 'skipped':
      return 'unknown';
    default: {
      const unhandled: never = check.check_conclusion;
      return unhandled;
    }
  }
}

export interface PullRequestIdentity {
  readonly base: string | null;
  readonly head: string | null;
  readonly mergeBase: string | null;
  readonly fetchedAtMicros: number | null;
  readonly outcome: DeliveryGitHubOperationSnapshotV1['outcome'] | null;
}

/** The identity the provider reported on its last complete `pull_request`
 * read; `null` fields when no complete read exists. Never falls back to the
 * latest attempt, an incomplete read does not name a merge base. */
export function providerIdentity(item: DeliveryPullRequestV1 | null): PullRequestIdentity {
  const snapshot =
    item?.operations.find((operation) => operation.operation === 'pull_request')?.last_complete ??
    null;
  return {
    base: snapshot?.provider_base_commit_id ?? null,
    head: snapshot?.provider_head_commit_id ?? null,
    mergeBase: snapshot?.merge_base_commit_id ?? null,
    fetchedAtMicros: snapshot?.fetched_at_micros ?? null,
    outcome: snapshot?.outcome ?? null,
  };
}
