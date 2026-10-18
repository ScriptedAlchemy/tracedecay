import type { DeliveryPullRequestV1 } from '../../contracts/generated.ts';

/** A pull request's repository number (`#741`); the provider's internal id is
 * never a display name. The number arrives with the identity read. */
export function pullRequestNumberLabel(pullRequest: DeliveryPullRequestV1): string {
  return pullRequest.identity === null ? 'number not served' : `#${pullRequest.identity.number}`;
}
