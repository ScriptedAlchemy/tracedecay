import type { DeliveryInboxPullRequestV1 } from '../../contracts/generated.ts';
import type { DeliveryContext } from './DeliveryPage.tsx';

export function ReviewWorkspace({
  context,
  row,
}: {
  context: DeliveryContext;
  row: DeliveryInboxPullRequestV1;
}) {
  return <div data-stub="review">{context.location.mode} {row.id}</div>;
}
