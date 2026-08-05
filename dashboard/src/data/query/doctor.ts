/** Read-only Doctor findings for the selected project scope. */
import {
  DoctorFindingsPayloadV1Schema,
  type DoctorFindingFamilyV1,
  type DoctorFindingsPayloadV1,
} from '../../contracts/generated.ts';
import { fetchEnvelope, type EnvelopeResult } from './envelope.ts';
import { scopeKey, scopedUrl, type DashboardScope } from '../scope/store.ts';

/** Scope-keyed so selecting a project cannot serve another project's cached
 * diagnosis while its own read is still in flight. */
export const doctorFindingsQueryKey = (scope: DashboardScope) =>
  ['doctor', 'findings', scopeKey(scope)] as const;

export function fetchDoctorFindings(
  scope: DashboardScope,
  family?: DoctorFindingFamilyV1,
): Promise<EnvelopeResult<DoctorFindingsPayloadV1>> {
  const query = family ? `?family=${encodeURIComponent(family)}` : '';
  return fetchEnvelope(
    scopedUrl(scope, `/api/doctor/findings${query}`),
    DoctorFindingsPayloadV1Schema,
  );
}
