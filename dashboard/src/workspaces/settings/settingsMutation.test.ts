import { afterEach, describe, expect, it, vi } from 'vitest';
import wire from '../../test/fixtures/settings-patch.json';
import { applySettingsMutation } from './settingsMutation.ts';

// runtime_surface_acceptance.rs captures the mounted daemon HTTP journey,
// including its durable commit, pending reconciliation, and refreshed revision.
const request = {
  writability: { state: 'writable', target: 'tracedecay' },
  scope: 'project',
  expectedRevisionId: wire.read.body.payload.project.configuration_revision_id,
  idempotencyKey: wire.changed.body.application_outcome.value.idempotency_key,
  readUrl: '/api/settings',
  patchUrl: '/api/settings/project',
  patch: {
    max_file_size: wire.changed.body.current.payload.project.config.max_file_size,
    include: wire.changed.body.current.payload.project.config.include,
    exclude: wire.changed.body.current.payload.project.config.exclude,
  },
} as const;

async function apply(
  response: { status: number; body: unknown },
  current: typeof wire.read.body | typeof wire.changed.body.current = wire.read.body,
) {
  const fetch = vi.fn()
    .mockResolvedValueOnce(Response.json(current, { status: wire.read.status }))
    .mockResolvedValueOnce(Response.json(response.body, { status: response.status }));
  vi.stubGlobal('fetch', fetch);
  const result = await applySettingsMutation({ ...request, expectedRevisionId: current.payload.project.configuration_revision_id });
  expect(fetch).toHaveBeenCalledTimes(2);
  return result;
}

afterEach(() => vi.unstubAllGlobals());

describe('settings producer/consumer settlement', () => {
  it('shows the real committed write as saved while reconciliation remains pending', async () => {
    expect(wire.changed.body.application_outcome.value.reconciliation).toBe('pending');
    expect(await apply(wire.changed)).toMatchObject({
      outcome: 'success',
      revisionId: wire.changed.body.current.payload.project.configuration_revision_id,
      effectReconciliation: 'pending',
      effectReceipt: wire.changed.body.application_outcome.value.receipt,
    });
  });

  it('accepts the explicit null outcome for an already-current patch', async () => {
    expect(await apply(wire.unchanged, wire.changed.body.current)).toMatchObject({
      outcome: 'success',
      revisionId: wire.unchanged.body.current.payload.project.configuration_revision_id,
      effectReceipt: null,
      effectReconciliation: null,
    });
  });

  it('requires the explicit nullable settlement and the canonical effect shape', async () => {
    for (const body of [
      { current: wire.changed.body.current },
      { ...wire.changed.body, application_outcome: { Effect: wire.changed.body.application_outcome.value } },
    ]) {
      expect(await apply({ status: 200, body })).toMatchObject({ outcome: 'protocol_error' });
    }
  });

  it('does not present uncertain or unproven effects as applied', async () => {
    for (const mutate of [
      (effect: typeof wire.changed.body.application_outcome.value) => { effect.execution.termination = 'effect_unknown'; },
      (effect: typeof wire.changed.body.application_outcome.value) => { effect.receipt.outcome = 'partial'; },
      (effect: typeof wire.changed.body.application_outcome.value) => { Object.assign(effect.receipt, { committed_state: null }); },
    ]) {
      const response = structuredClone(wire.changed);
      mutate(response.body.application_outcome.value);
      expect(await apply(response)).toMatchObject({ outcome: 'unsettled', detail: expect.stringContaining('The settings change may have been applied') });
    }
  });

  it('requires the returned current revision to reflect the committed change', async () => {
    const response = structuredClone(wire.changed);
    response.body.current.payload.project.configuration_revision_id = request.expectedRevisionId;
    expect(await apply(response)).toMatchObject({ outcome: 'unsettled' });
  });

  it('retains validation, revision conflicts and unavailable authority semantics', async () => {
    expect(await apply(wire.validation)).toMatchObject({ outcome: 'validation', errors: wire.validation.body.validation_errors });
    expect(await apply(wire.revision_conflict)).toEqual({
      outcome: 'conflict',
      expectedRevisionId: wire.revision_conflict.body.expected_revision_id,
      actualRevisionId: wire.revision_conflict.body.actual_revision_id,
    });
    expect(await apply(wire.unavailable)).toMatchObject({ outcome: 'unavailable' });
  });

  it('keeps idempotency conflict and cancellation messages distinct from revision conflicts', async () => {
    for (const response of [wire.idempotency_conflict, wire.cancelled]) {
      expect(await apply(response)).toEqual({ outcome: 'error', detail: response.body.problem.message });
    }
  });

  it('requires current values before retrying a problem carrying a committed effect', async () => {
    const response = structuredClone(wire.idempotency_conflict);
    Object.assign(response.body.problem, {
      kind: 'partial_effect',
      committed_receipt: { ...wire.changed.body.application_outcome.value.receipt, outcome: 'partial' },
    });
    expect(await apply(response)).toMatchObject({ outcome: 'unsettled', detail: expect.stringContaining('The settings change may have been applied') });
  });

  it('treats a lost PATCH response as uncertain and requires readback before another save', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(Response.json(wire.read.body, { status: wire.read.status }))
      .mockRejectedValueOnce(new Error('connection lost'));
    vi.stubGlobal('fetch', fetch);

    expect(await applySettingsMutation(request)).toEqual({
      outcome: 'unsettled',
      detail: expect.stringContaining('The change may have been applied'),
    });
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('rejects problem/status disagreement and malformed refusals', async () => {
    expect(await apply({ ...wire.idempotency_conflict, status: 503 })).toMatchObject({ outcome: 'protocol_error' });
    expect(await apply({ status: 409, body: {} })).toMatchObject({ outcome: 'protocol_error' });
  });
});
