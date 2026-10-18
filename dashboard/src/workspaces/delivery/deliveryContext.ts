import { DeliveryOverviewV1Schema, type DeliveryInboxV1 } from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import type { DeliveryLocation, DeliveryLocationPatch } from './deliveryLocation.ts';
import type { UmbrellaProjection } from './umbrella.ts';

/** What every Delivery workspace reads: the served inbox, the umbrella
 * projection derived from it, the URL location, and the one way to change it. */
export interface DeliveryContext {
  readonly inbox: DeliveryInboxV1;
  readonly umbrellas: UmbrellaProjection;
  readonly location: DeliveryLocation;
  readonly params: URLSearchParams;
  readonly navigate: (patch: DeliveryLocationPatch) => void;
}

/** The project-scoped overview read. `/api/projects/{id}/…` is never rewritten
 * by the shell scope, so the selected PR's project is always the one read. */
export function useProjectOverview(projectId: string | null) {
  return useEnvelope(
    ['delivery', 'overview', projectId ?? 'none'],
    `/api/projects/${encodeURIComponent(projectId ?? '')}/delivery/overview`,
    DeliveryOverviewV1Schema,
    { enabled: projectId !== null },
  );
}
