import type {
  DeliveryInboxProjectV1,
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
  DeliveryMembershipEdgeV1,
} from '../../contracts/generated.ts';
import type { DeliveryLocation } from './deliveryLocation.ts';

/** The inbox rows the current filters admit. Filters only narrow; they never
 * reorder the daemon's deterministic project/PR ordering. */
export function filterInbox(
  inbox: DeliveryInboxV1,
  location: DeliveryLocation,
): readonly DeliveryInboxPullRequestV1[] {
  const providerByProject = new Map(
    inbox.projects.map((project) => [project.project_id, project.provider_state]),
  );
  return inbox.pull_requests.filter((row) => {
    if (location.project !== null && row.project_id !== location.project) return false;
    if (
      location.attention !== null &&
      !row.attention.some((item) => item.source === location.attention)
    ) {
      return false;
    }
    if (location.status !== null) {
      const identity = row.pull_request.identity;
      if (identity === null) return false;
      if (location.status === 'draft' ? !identity.draft : identity.state !== location.status) {
        return false;
      }
    }
    if (
      location.provider !== null &&
      providerByProject.get(row.project_id) !== location.provider
    ) {
      return false;
    }
    if (location.unresolvedOnly && !row.attention.some((item) => item.state === 'active')) {
      return false;
    }
    return true;
  });
}

export function edgesFor(
  inbox: DeliveryInboxV1,
  row: DeliveryInboxPullRequestV1,
): readonly DeliveryMembershipEdgeV1[] {
  return inbox.membership_edges.filter(
    (edge) =>
      edge.project_id === row.project_id &&
      edge.pull_request_id === row.pull_request.pull_request_id,
  );
}

export function projectFor(
  inbox: DeliveryInboxV1,
  projectId: string | null,
): DeliveryInboxProjectV1 | null {
  if (projectId === null) return null;
  return inbox.projects.find((project) => project.project_id === projectId) ?? null;
}

export function activeAttention(row: DeliveryInboxPullRequestV1): number {
  return row.attention.filter((item) => item.state === 'active').length;
}
