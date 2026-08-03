/**
 * Doctor reads and owner-remediation controls, in the selected scope.
 *
 * Every route here used to be requested unprefixed, which meant the Doctor
 * panel read the *active* project while the storage telemetry, storage
 * findings and canonical observations beside it — and the app-wide health dot
 * in the rail — all read the selected one. A reader who selected a project got
 * one panel's diagnosis of a different project, with nothing on screen saying
 * so. All four routes are mounted in the daemon's `project_api_router`, so the
 * fix is to ask for the scope the rest of the workspace is showing.
 *
 * The gateway serves those reads for any registered project and refuses the
 * two remediation writes for every project that is not the active one. So the
 * reads are simply scoped, and the writes are gated on the same scope
 * authority the controls disable themselves with.
 */
import {
  assertNever,
  DoctorFindingsPayloadV1Schema,
  DoctorRemediationApplyRequestV1Schema,
  DoctorRemediationPayloadV1Schema,
  DoctorRemediationPreviewRequestV1Schema,
  type DoctorFindingFamilyV1,
  type DoctorFindingsPayloadV1,
  type DoctorRemediationApplyRequestV1,
  type DoctorRemediationPayloadV1,
  type DoctorRemediationPreviewRequestV1,
} from '../../contracts/generated.ts';
import { fetchEnvelope, type EnvelopeResult } from './envelope.ts';
import {
  scopeKey,
  scopeWritable,
  scopedUrl,
  type DashboardScope,
  type ScopeWritability,
} from '../scope/store.ts';

/** Scope-keyed, so selecting a project cannot serve another project's cached
 * diagnosis while its own read is still in flight. */
export const doctorFindingsQueryKey = (scope: DashboardScope) =>
  ['doctor', 'findings', scopeKey(scope)] as const;

export const doctorOperationQueryKey = (scope: DashboardScope, operationId: string) =>
  ['doctor', 'remediation', scopeKey(scope), operationId] as const;

/**
 * What a remediation control produced, including the case where it declined to
 * dispatch.
 *
 * `not_dispatched` is the absence of a write rather than a failed one, and it
 * stays apart from every transport state for that reason: no request was made,
 * so no store was touched, and the surface must not imply the daemon was asked
 * and said no.
 */
export type DoctorWriteResult =
  | EnvelopeResult<DoctorRemediationPayloadV1>
  /** `writable` is excluded rather than merely unused: a dispatch that did not
   * happen always has a reason, so the type carries one instead of leaving the
   * surface to handle a case the control cannot produce. */
  | {
      outcome: 'not_dispatched';
      writability: Exclude<ScopeWritability, { state: 'writable' }>;
    };

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

export function previewDoctorRemediation(
  scope: DashboardScope,
  request: DoctorRemediationPreviewRequestV1,
): Promise<DoctorWriteResult> {
  const body = DoctorRemediationPreviewRequestV1Schema.parse(request);
  return dispatchRemediation(scope, '/api/doctor/remediations/preview', body);
}

export function applyDoctorRemediation(
  scope: DashboardScope,
  request: DoctorRemediationApplyRequestV1,
): Promise<DoctorWriteResult> {
  const body = DoctorRemediationApplyRequestV1Schema.parse(request);
  return dispatchRemediation(scope, '/api/doctor/remediations/apply', body);
}

/**
 * Issue a remediation, or decline to.
 *
 * A preview is a write as far as the gateway is concerned — it is a POST — and
 * it is treated as one here rather than being let through on the grounds that
 * it only inspects. That is the daemon's judgement to make, not this module's.
 */
async function dispatchRemediation(
  scope: DashboardScope,
  route: string,
  body: unknown,
): Promise<DoctorWriteResult> {
  const writability = scopeWritable(scope);
  // Nothing leaves the browser unless this scope is known to accept it. An
  // unresolved scope is included: "not established yet" is not permission, and
  // a remediation is the last control in this product that should be dispatched
  // on an assumption.
  switch (writability.state) {
    case 'read_only':
    case 'unknown':
      return { outcome: 'not_dispatched', writability };
    case 'writable':
      break;
    default:
      return assertNever(writability);
  }
  return fetchEnvelope(scopedUrl(scope, route), DoctorRemediationPayloadV1Schema, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(body),
  });
}

export function fetchDoctorRemediationStatus(
  scope: DashboardScope,
  operationId: string,
): Promise<EnvelopeResult<DoctorRemediationPayloadV1>> {
  return fetchEnvelope(
    scopedUrl(scope, `/api/doctor/remediations/${encodeURIComponent(operationId)}`),
    DoctorRemediationPayloadV1Schema,
  );
}

const jsonHeaders = {
  accept: 'application/json',
  'content-type': 'application/json',
} as const;
