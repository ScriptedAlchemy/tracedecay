import { describe, expect, it } from 'vitest';
import {
  DoctorFindingsPayloadV1Schema,
  DoctorRemediationPayloadV1Schema,
  type DashboardDoctorRemediationDescriptorV1,
} from '../../contracts/generated.ts';
import {
  availableRemediationActions,
  doctorEvidencePresentation,
  doctorOwningSurfaceLabel,
  readActiveDoctorOperation,
  saveActiveDoctorOperation,
  sameDoctorScope,
} from './doctorModel.ts';

const descriptor: DashboardDoctorRemediationDescriptorV1 = {
  operation: 'use-case.application.configuration.pin-authority',
  surface: 'configuration_control_plane',
  preview_available: true,
  action_confirmation: 'required',
  summary: 'apply the admitted configuration revision',
  target: { owner_operation: 'configuration_pin_authority' },
};
const projectAuthorityScope = {
  project_id: 'project.doctor',
  repository_id: 'repository.doctor',
  worktree_id: 'worktree.doctor',
  reference: null,
  scope_digest: `sha256:${'a'.repeat(64)}`,
};

describe('Doctor frontend contracts', () => {
  it('decodes canonical finding evidence, coverage, and remediation metadata', () => {
    const payload = DoctorFindingsPayloadV1Schema.parse({
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
      DoctorRemediationPayloadV1Schema.parse({
        status: 'unavailable',
        reason: 'denied',
      }),
    ).toEqual({ status: 'unavailable', reason: 'denied' });

    const preview = DoctorRemediationPayloadV1Schema.parse({
      status: 'operation',
      operation: {
        operation_id: 'request.doctor.preview',
        owning_operation: descriptor.operation,
        phase: 'previewed',
        preview_id: 'preview.doctor.preview',
        idempotency_key: null,
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
        verification: { state: 'not_required' },
      },
    });

    expect(preview.status).toBe('operation');
    if (preview.status === 'operation') {
      expect(preview.operation.preview_id).toBe('preview.doctor.preview');
    }
  });

  it('decodes and binds the owner effect receipt scope', () => {
    const payload = DoctorRemediationPayloadV1Schema.parse({
      status: 'operation',
      operation: {
        operation_id: 'request.doctor.storage-collect',
        owning_operation: 'use-case.application.storage.collect-orphan-store',
        phase: 'completed',
        preview_id: 'preview.doctor.storage-collect',
        idempotency_key: 'idempotency.doctor.storage-collect',
        execution: {
          started_at: 1,
          ended_at: 2,
          effective_deadline: { expires_at: 10 },
          cancellation: null,
          budget: {
            units_consumed: 1,
            bytes_consumed: 1,
            elapsed_micros: 1,
          },
          termination: 'completed',
        },
        effect_receipt: {
          operation: 'use-case.application.storage.collect-orphan-store',
          request_id: 'request.doctor.storage-collect',
          actor: 'actor.tracedecay-dashboard',
          scope: projectAuthorityScope,
          effect_class: 'administrative',
          idempotency_key: 'idempotency.doctor.storage-collect',
          input_digest: `sha256:${'d'.repeat(64)}`,
          expected_state: `sha256:${'e'.repeat(64)}`,
          policy_digest: `sha256:${'f'.repeat(64)}`,
          configuration_digest: `sha256:${'1'.repeat(64)}`,
          catalog_digest: `sha256:${'2'.repeat(64)}`,
          privacy_digest: `sha256:${'3'.repeat(64)}`,
          outcome: 'completed',
          committed_state: `sha256:${'4'.repeat(64)}`,
          external_proof: null,
        },
        verification: { state: 'unavailable' },
      },
    });

    expect(payload.status).toBe('operation');
    if (payload.status === 'operation') {
      expect(payload.operation.effect_receipt?.scope.project_id).toBe('project.doctor');
    }
  });

  it('never invents preview or apply authority from descriptor metadata', () => {
    expect(availableRemediationActions(descriptor, [])).toEqual({
      canPreview: false,
      canApply: false,
      dispatchable: true,
    });
    expect(
      availableRemediationActions(descriptor, [
        {
          kind: 'request_apply',
          operation: 'use-case.application.runtime.recover-daemon',
        },
      ]),
    ).toEqual({ canPreview: false, canApply: false, dispatchable: true });
    expect(
      availableRemediationActions(descriptor, [
        { kind: 'request_dry_run', operation: descriptor.operation },
        { kind: 'request_apply', operation: descriptor.operation },
      ]),
    ).toEqual({ canPreview: true, canApply: true, dispatchable: true });
  });

  it('keeps an owner-authorized action distinct from one this view can address', () => {
    // The findings route sends `target: null` for an operation whose change it
    // cannot determine from the finding, while the owner still authorizes the
    // apply. Reading that as "unauthorized" would deny an available repair.
    const withoutTarget: DashboardDoctorRemediationDescriptorV1 = {
      ...descriptor,
      operation: 'use-case.application.configuration.protected-apply',
      target: null,
    };
    expect(
      availableRemediationActions(withoutTarget, [
        { kind: 'request_apply', operation: withoutTarget.operation },
      ]),
    ).toEqual({ canPreview: false, canApply: true, dispatchable: false });
    expect(doctorOwningSurfaceLabel(withoutTarget.surface)).toBe(
      'the configuration control plane',
    );
  });

  it('persists only the durable operation identity for reload resume', () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    };

    const active = {
      schema_revision: 3 as const,
      operation_id: 'request.doctor.resume',
      transport_scope: {
        project_id: 'project.doctor',
        storage_mode: 'project_local',
        store_root: '/project',
      },
    };
    saveActiveDoctorOperation(active, storage);
    expect(readActiveDoctorOperation(storage)).toEqual(active);
    expect(sameDoctorScope(active.transport_scope, { ...active.transport_scope })).toBe(true);
    expect(
      sameDoctorScope(active.transport_scope, {
        ...active.transport_scope,
        project_id: 'project.other',
      }),
    ).toBe(false);
  });
});
