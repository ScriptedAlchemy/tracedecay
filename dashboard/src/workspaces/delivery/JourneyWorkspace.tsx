import type { DeliveryInboxPullRequestV1, DeliveryMembershipEdgeV1 } from '../../contracts/generated.ts';
import type { DeliveryContext } from './DeliveryPage.tsx';

export function JourneyWorkspace({
  context,
  row,
  edges,
}: {
  context: DeliveryContext;
  row: DeliveryInboxPullRequestV1;
  edges: readonly DeliveryMembershipEdgeV1[];
}) {
  return <div data-stub="journey">{context.location.mode} {row.id} {edges.length}</div>;
}
