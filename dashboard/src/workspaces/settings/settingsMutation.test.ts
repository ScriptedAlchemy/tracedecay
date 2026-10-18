import { afterEach, describe, expect, it, vi } from 'vitest';
import wire from '../../test/fixtures/settings-patch.json';
import { applySettingsMutation } from './settingsMutation.ts';

// settings_api/tests/wire.rs serializes these responses through Axum. Keep
// the complete receipts so decoding exercises the producer contract.
const request = {
  writability: { state: 'writable', target: 'tracedecay' },
  scope: 'project',
  expectedRevisionId: 'rev-42',
  idempotencyKey: 'settings-idempotency-fixture',
  readUrl: '/api/settings',
  patchUrl: '/api/settings/project',
  patch: { max_file_size: 2_097_152 },
} as const;

async function apply(response: { status: number; body: unknown }) {
  const fetch = vi.fn()
    .mockResolvedValueOnce(Response.json(wire.read.body, { status: wire.read.status }))
    .mockResolvedValueOnce(Response.json(response.body, { status: response.status }));
  vi.stubGlobal('fetch', fetch);
  const result = await applySettingsMutation(request);
  expect(fetch).toHaveBeenCalledTimes(2);
  return result;
}

afterEach(() => vi.unstubAllGlobals());

describe('settings producer/consumer settlement', () => {
  it('accepts the committed effect and preserves its complete durable receipt', async () => {
    expect(await apply(wire.changed)).toMatchObject({
      outcome: 'success',
      revisionId: 'rev-43',
      effectReceipt: wire.changed.body.application_outcome.value.receipt,
    });
  });

  it('accepts the explicit null outcome for an already-current patch', async () => {
    expect(await apply(wire.unchanged)).toMatchObject({ outcome: 'success', revisionId: 'rev-42', effectReceipt: null });
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
      (effect: typeof wire.changed.body.application_outcome.value) => { effect.reconciliation = 'pending'; },
      (effect: typeof wire.changed.body.application_outcome.value) => { effect.receipt.outcome = 'partial'; },
      (effect: typeof wire.changed.body.application_outcome.value) => { Object.assign(effect.receipt, { committed_state: null }); },
    ]) {
      const response = structuredClone(wire.changed);
      mutate(response.body.application_outcome.value);
      expect(await apply(response)).toMatchObject({ outcome: 'unsettled', detail: expect.stringContaining('Refresh current values before retrying') });
    }
  });

  it('retains validation, revision conflicts and unavailable authority semantics', async () => {
    expect(await apply(wire.validation)).toMatchObject({ outcome: 'validation', errors: wire.validation.body.validation_errors });
    expect(await apply(wire.revision_conflict)).toEqual({ outcome: 'conflict', expectedRevisionId: 'rev-42', actualRevisionId: 'rev-43' });
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
    expect(await apply(response)).toMatchObject({ outcome: 'unsettled', detail: expect.stringContaining('Refresh current values before retrying') });
  });

  it('rejects problem/status disagreement and malformed refusals', async () => {
    expect(await apply({ ...wire.idempotency_conflict, status: 503 })).toMatchObject({ outcome: 'protocol_error' });
    expect(await apply({ status: 409, body: {} })).toMatchObject({ outcome: 'protocol_error' });
  });
});
