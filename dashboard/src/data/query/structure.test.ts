/**
 * The four outcomes must stay four.
 *
 * Every assertion here is a falsified-UI guard rather than a shape check: the
 * failure this file exists to catch is a refactor that collapses `unmeasured`
 * or `failed` into an empty measurement, which would turn "the producer did not
 * run" into "the producer ran and found nothing" everywhere these routes are
 * drawn. Each case therefore asserts the negative — never `measured` — as well
 * as the positive.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { z } from 'zod';

import { absenceReason, fetchStructure } from './structure.ts';

const MeasurementSchema = z.object({ hop_count: z.number().int() });
type Measurement = z.infer<typeof MeasurementSchema>;

const ReadSchema = z.discriminatedUnion('status', [
  z.object({
    status: z.literal('failed'),
    code: z.string(),
    detail: z.string(),
    retryable: z.boolean(),
  }),
  z.object({ status: z.literal('measured'), measurement: MeasurementSchema }),
  z.object({ status: z.literal('unmeasured'), reason: z.string(), detail: z.string() }),
]) as unknown as z.ZodType<
  | { status: 'measured'; measurement: Measurement }
  | { status: 'unmeasured'; reason: string; detail: string }
  | { status: 'failed'; code: string; detail: string; retryable: boolean }
>;

/** A complete envelope, so the reader is exercised against the real wrapper
 * rather than a payload the schema would have rejected in production. */
function envelope(payload: unknown): unknown {
  return {
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      denominator: null,
      eligible: 1,
      examined: null,
      excluded: null,
      matched: null,
      omission_reasons: [],
      omitted: null,
      unit: 'paths',
      unknown: null,
    },
    domain_state: 'ready',
    freshness: { observed_at_micros: null, state: 'fresh', watermark: null },
    legal_actions: [],
    payload,
    schema_revision: 1,
    scope: { project_id: 'proj_test', storage_mode: 'project', store_root: '/tmp/store' },
    source_watermark: null,
    time: { observation_time_micros: 0, valid_time_micros: null },
    version: { entity_version: null, graph_version: null },
  };
}

function respond(body: unknown, init?: { ok?: boolean; status?: number }) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: init?.ok ?? true,
      status: init?.status ?? 200,
      json: async () => body,
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('fetchStructure', () => {
  it('passes a measured reading through unchanged', async () => {
    respond(envelope({ status: 'measured', measurement: { hop_count: 3 } }));
    const result = await fetchStructure<Measurement>('/x', ReadSchema);
    expect(result.outcome).toBe('measured');
    expect(result).toMatchObject({ measurement: { hop_count: 3 } });
    expect(absenceReason(result)).toBeNull();
  });

  it('keeps an unmeasured read distinct from an empty measurement', async () => {
    respond(
      envelope({
        status: 'unmeasured',
        reason: 'graph_authority_unavailable',
        detail: 'the retained project graph is unavailable',
      }),
    );
    const result = await fetchStructure<Measurement>('/x', ReadSchema);
    expect(result.outcome).toBe('unmeasured');
    // The guard that matters: nothing downstream can read a measurement off it.
    expect(result).not.toHaveProperty('measurement');
    expect(absenceReason(result)).toContain('graph_authority_unavailable');
  });

  it('keeps a producer failure distinct from an unmeasured read', async () => {
    respond(
      envelope({
        status: 'failed',
        code: 'session_node_read_failed',
        detail: 'db locked',
        retryable: true,
      }),
    );
    const result = await fetchStructure<Measurement>('/x', ReadSchema);
    expect(result.outcome).toBe('failed');
    expect(result).toMatchObject({ retryable: true });
    expect(absenceReason(result)).toContain('retryable');
  });

  it('reports an unreachable route as transport, not as a producer failure', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('ECONNREFUSED')));
    const result = await fetchStructure<Measurement>('/x', ReadSchema);
    expect(result).toEqual({ outcome: 'transport', state: 'offline' });
    // A dead daemon must never be attributed to the producer.
    expect(result.outcome).not.toBe('failed');
  });

  it('reports a non-2xx as transport with its status', async () => {
    respond({}, { ok: false, status: 503 });
    const result = await fetchStructure<Measurement>('/x', ReadSchema);
    expect(result).toEqual({ outcome: 'transport', state: 'error', detail: 'HTTP 503' });
  });

  it('keeps the typed reading when a non-2xx still carries a full envelope', async () => {
    // The strata route answers 503 while the verified graph is warming, but
    // its body is a complete typed envelope with the producer's own reason.
    // That reason must reach the surface instead of a raw `error — HTTP 503`.
    respond(
      envelope({
        status: 'unmeasured',
        reason: 'graph_authority_unavailable',
        detail: 'the verified code graph is not ready for this worktree',
      }),
      { ok: false, status: 503 },
    );
    const result = await fetchStructure<Measurement>('/x', ReadSchema);
    expect(result.outcome).toBe('unmeasured');
    expect(absenceReason(result)).toContain('graph_authority_unavailable');
    expect(absenceReason(result)).not.toContain('HTTP 503');
  });

  it('refuses a body that does not match the contract instead of inventing one', async () => {
    respond(envelope({ status: 'measured', measurement: { hop_count: 'three' } }));
    const result = await fetchStructure<Measurement>('/x', ReadSchema);
    expect(result).toEqual({ outcome: 'transport', state: 'unsupported_schema' });
    expect(result.outcome).not.toBe('measured');
  });
});
