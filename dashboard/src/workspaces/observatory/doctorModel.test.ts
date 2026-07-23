import { describe, expect, it } from 'vitest';
import {
  DoctorFindingsPayloadSchema,
  DoctorRemediationPayloadSchema,
  type DoctorRemediationDescriptor,
} from '../../contracts/wire.ts';
import {
  availableRemediationActions,
  doctorEvidencePresentation,
  readActiveDoctorOperation,
  saveActiveDoctorOperation,
  sameDoctorScope,
} from './doctorModel.ts';

const descriptor: DoctorRemediationDescriptor = {
  operation: 'use-case.application.configuration.protected-apply',
  surface: 'configuration_control_plane',
  preview_available: true,
  action_confirmation: 'required',
  summary: 'apply the admitted configuration revision',
};

describe('Doctor frontend contracts', () => {
  it('decodes canonical finding evidence, coverage, and remediation metadata', () => {
    const payload = DoctorFindingsPayloadSchema.parse({
      family_filter: null,
      entries: [
        {
          finding: {
            family: 'configuration',
            state: 'degraded',
            evidence: [
              {
                family: 'configuration',
                reference: 'configuration:desired-effective-drift',
              },
            ],
            coverage: {
              completeness: 'complete',
              statement: 'configuration authority was consulted',
            },
            remediation: {
              owning_operation: descriptor.operation,
              kind: 'action',
            },
          },
          storage_kind: null,
        },
      ],
      report_coverage: {
        families: [
          {
            family: 'configuration',
            consultation: { status: 'consulted' },
          },
          {
            family: 'semantic_index',
            consultation: { status: 'unavailable', reason: 'unsupported' },
          },
        ],
        completeness: 'partial',
        statement: {
          completeness: 'partial',
          statement: 'one family was unavailable',
        },
      },
      remediations: [descriptor],
      known_families: ['configuration', 'semantic_index'],
      note: 'one family was unavailable',
    });

    expect(payload.entries[0]?.finding.evidence[0]?.reference).toBe(
      'configuration:desired-effective-drift',
    );
    expect(payload.report_coverage?.families[1]?.consultation).toEqual({
      status: 'unavailable',
      reason: 'unsupported',
    });
    expect(doctorEvidencePresentation(payload.entries[0]!.finding.state).label).toBe(
      'Degraded',
    );
  });

  it('decodes typed remediation unavailable and operation outcomes', () => {
    expect(
      DoctorRemediationPayloadSchema.parse({
        status: 'unavailable',
        reason: 'denied',
      }),
    ).toEqual({ status: 'unavailable', reason: 'denied' });

    const preview = DoctorRemediationPayloadSchema.parse({
      status: 'operation',
      operation: {
        operation_id: 'request.doctor.preview',
        owning_operation: descriptor.operation,
        phase: 'previewed',
        preview_id: 'preview.doctor.preview',
        execution: {
          started_at: 1,
          ended_at: 2,
          effective_deadline: { expires_at: 10 },
          cancellation: null,
          budget: {
            units_consumed: 1,
            bytes_consumed: 0,
            elapsed_micros: 1,
          },
          termination: 'completed',
        },
        effect_receipt: null,
      },
    });

    expect(preview.status).toBe('operation');
    if (preview.status === 'operation') {
      expect(preview.operation.preview_id).toBe('preview.doctor.preview');
    }
  });

  it('never invents preview or apply authority from descriptor metadata', () => {
    expect(availableRemediationActions(descriptor, [])).toEqual({
      canPreview: false,
      canApply: false,
    });
    expect(
      availableRemediationActions(descriptor, [
        {
          kind: 'request_apply',
          operation: 'use-case.application.runtime.recover-daemon',
        },
      ]),
    ).toEqual({ canPreview: false, canApply: false });
    expect(
      availableRemediationActions(descriptor, [
        { kind: 'request_dry_run', operation: descriptor.operation },
        { kind: 'request_apply', operation: descriptor.operation },
      ]),
    ).toEqual({ canPreview: true, canApply: true });
  });

  it('persists only the durable operation identity for reload resume', () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    };

    const active = {
      schema_revision: 1 as const,
      operation_id: 'request.doctor.resume',
      scope: {
        project_id: 'project.doctor',
        storage_mode: 'project_local',
        store_root: '/project',
      },
    };
    saveActiveDoctorOperation(active, storage);
    expect(readActiveDoctorOperation(storage)).toEqual(active);
    expect(sameDoctorScope(active.scope, { ...active.scope })).toBe(true);
    expect(
      sameDoctorScope(active.scope, {
        ...active.scope,
        project_id: 'project.other',
      }),
    ).toBe(false);
  });
});
