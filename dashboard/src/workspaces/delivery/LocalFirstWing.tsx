import type { DeliveryInboxProjectV1, DeliveryOverviewV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { UseQueryResult } from '@tanstack/react-query';

export function LocalFirstWing({
  project,
  overview,
}: {
  project: DeliveryInboxProjectV1;
  overview: UseQueryResult<EnvelopeResult<DeliveryOverviewV1>>;
}) {
  return <div data-stub="local-first">{project.project_id} {String(overview.isPending)}</div>;
}
