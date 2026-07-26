import {
  DoctorFindingsPayloadSchema,
  DoctorRemediationApplyRequestSchema,
  DoctorRemediationPayloadSchema,
  DoctorRemediationPreviewRequestSchema,
  type DoctorFindingFamily,
  type DoctorFindingsPayload,
  type DoctorRemediationApplyRequest,
  type DoctorRemediationPayload,
  type DoctorRemediationPreviewRequest,
} from '../../contracts/wire.ts';
import { fetchEnvelope, type EnvelopeResult } from './envelope.ts';

export const doctorFindingsQueryKey = ['doctor', 'findings'] as const;
export const doctorOperationQueryKey = (operationId: string) =>
  ['doctor', 'remediation', operationId] as const;

export function fetchDoctorFindings(
  family?: DoctorFindingFamily,
): Promise<EnvelopeResult<DoctorFindingsPayload>> {
  const query = family ? `?family=${encodeURIComponent(family)}` : '';
  return fetchEnvelope(`/api/doctor/findings${query}`, DoctorFindingsPayloadSchema);
}

export function previewDoctorRemediation(
  request: DoctorRemediationPreviewRequest,
): Promise<EnvelopeResult<DoctorRemediationPayload>> {
  const body = DoctorRemediationPreviewRequestSchema.parse(request);
  return fetchEnvelope('/api/doctor/remediations/preview', DoctorRemediationPayloadSchema, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(body),
  });
}

export function applyDoctorRemediation(
  request: DoctorRemediationApplyRequest,
): Promise<EnvelopeResult<DoctorRemediationPayload>> {
  const body = DoctorRemediationApplyRequestSchema.parse(request);
  return fetchEnvelope('/api/doctor/remediations/apply', DoctorRemediationPayloadSchema, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(body),
  });
}

export function fetchDoctorRemediationStatus(
  operationId: string,
): Promise<EnvelopeResult<DoctorRemediationPayload>> {
  return fetchEnvelope(
    `/api/doctor/remediations/${encodeURIComponent(operationId)}`,
    DoctorRemediationPayloadSchema,
  );
}

const jsonHeaders = {
  accept: 'application/json',
  'content-type': 'application/json',
} as const;
