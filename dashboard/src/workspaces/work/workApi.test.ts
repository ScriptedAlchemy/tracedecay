/**
 * The Work transport, against the envelope the daemon really sends.
 *
 * Two things are being guarded here. The first is that a refusal never becomes
 * a read: every non-2xx status, every malformed envelope and every payload the
 * generated schema rejects has to arrive as a stated reason, because a Work
 * board that renders an empty set on a 503 is the exact failure this workspace
 * was built to avoid.
 *
 * The second is the envelope walk itself. The dashboard has no generated schema
 * for the application envelope, so these fixtures are written to match
 * `HttpJsonEnvelope` in `crates/tracedecay-api/src/lib.rs` field for field. If
 * the daemon's wrapper moves, the walk must miss and report
 * `unsupported_schema` — never reach a stale field and call it a value.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';

import { workGraphRead } from '../../test/workGraphFixture.ts';
import { callWork, workPayload, workRefusal } from './workApi.ts';
import {
  WORK_PREPARE_GRAPH_MUTATION_ROUTE,
  WORK_VIEWS_ROUTE,
} from './workRoutes.ts';

/** A product graph payload and request that satisfy the generated contracts. */
const GRAPH = workGraphRead({ tasks: [] });
const GRAPH_REQUEST = {
  selection: { selection: 'profile_owned_no_git' as const },
  mode: { mode: 'current' as const },
  continuation: null,
  observed_at: 1,
};
const PREPARE_REQUEST = {
  causation_event_id: null,
  evidence: [],
  selection: { selection: 'profile_owned_no_git' as const },
  change: {
    change: 'accept_task' as const,
    evidence_by_criterion: {},
    task_id: 'task-1',
  },
};

const RESOLVED_SCOPE = {
  project_id: 'project.work',
  repository_id: 'repository.work',
  worktree_id: 'worktree.work',
  reference: null,
  scope_digest: 'sha256:scope',
};

/** The daemon's success envelope: `kind`/`value`, then the outcome tag, then
 * the packet whose `payload` holds the contract. */
function success(payload: unknown, outcome: 'evidence' | 'effect' = 'evidence') {
  return {
    kind: 'success',
    value: {
      binding_id: 'binding.http.work.snapshot',
      contract: { schema_id: 'schema.work.snapshot.result', schema_revision: 1 },
      request_id: 'request-1',
      scope: RESOLVED_SCOPE,
      outcome: { outcome, value: { payload } },
    },
  };
}

function stub(status: number, body: unknown, options: { invalidJson?: boolean } = {}): void {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(options.invalidJson === true ? '<html/>' : JSON.stringify(body), {
          status,
          headers: { 'content-type': 'application/json' },
        }),
    ),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('the Work problem taxonomy', () => {
  /**
   * Each status the daemon's `application_problem_status` can produce, mapped
   * to the state the page will render. The mapping is asserted whole because a
   * single wrong entry is invisible in the UI — it just reads as the wrong
   * reason — and 409 in particular has to be distinguishable from 503, since
   * one means read again and the other means nothing is running.
   */
  it('gives every daemon problem status its own reading', () => {
    expect(workRefusal(400).state).toBe('error');
    expect(workRefusal(404).state).toBe('denied');
    expect(workRefusal(405).state).toBe('locked');
    expect(workRefusal(409).state).toBe('conflicting');
    expect(workRefusal(422).state).toBe('unsupported');
    expect(workRefusal(429).state).toBe('unavailable');
    expect(workRefusal(408).state).toBe('cancelled');
    expect(workRefusal(503).state).toBe('unavailable');
    expect(workRefusal(504).state).toBe('timed_out');
  });

  it('never reports a refusal without a reason', () => {
    for (const status of [400, 404, 405, 408, 409, 422, 429, 500, 503, 504]) {
      expect(workRefusal(status).detail, `HTTP ${status} carries no reason`).not.toBe('');
    }
  });
});

describe('the application envelope walk', () => {
  it('finds the contract inside an evidence packet and an effect result', () => {
    expect(workPayload(success(GRAPH))).toEqual({ found: true, payload: GRAPH });
    expect(workPayload(success(GRAPH, 'effect'))).toEqual({ found: true, payload: GRAPH });
  });

  /**
   * Each of these is a wrapper the daemon does not send. The walk has to miss
   * rather than reach past the difference, because reaching past it is how a
   * changed envelope turns into a confidently wrong read.
   */
  it('misses anything that is not that envelope', () => {
    expect(workPayload(undefined).found).toBe(false);
    expect(workPayload({ kind: 'problem', value: { problem: {} } }).found).toBe(false);
    expect(workPayload({ kind: 'success' }).found).toBe(false);
    expect(workPayload({ kind: 'success', value: { outcome: {} } }).found).toBe(false);
    // The packet is present but carries no payload field at all.
    expect(
      workPayload({ kind: 'success', value: { outcome: { outcome: 'evidence', value: {} } } })
        .found,
    ).toBe(false);
    // The payload itself is a bare value rather than the wrapped packet.
    expect(workPayload({ kind: 'success', value: { outcome: GRAPH } }).found).toBe(false);
  });

  /** `null` is the daemon saying the operation carried no value. That is found
   * — the envelope was well-formed — and is refused a step later, so it can
   * never be confused with a missing envelope. */
  it('separates an absent payload from an absent envelope', () => {
    expect(workPayload(success(null))).toEqual({ found: true, payload: null });
  });
});

describe('callWork', () => {
  it('returns the payload decoded by its generated schema', async () => {
    stub(200, success(GRAPH));
    const result = await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views');
    expect(result).toEqual({ outcome: 'value', value: GRAPH, scope: RESOLVED_SCOPE });
  });

  it('refuses a success envelope without a valid resolved scope', async () => {
    const body = success(GRAPH);
    stub(200, { ...body, value: { ...body.value, scope: {} } });

    const result = await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views');

    expect(result).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  it('refuses a payload the generated contract does not describe', async () => {
    // A graph missing its authorized scope is exactly what a drifted daemon
    // would send, and rendering it would draw a board with no selection authority.
    stub(200, success({ mode: 'current', snapshot: GRAPH.snapshot }));
    const result = await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views');
    expect(result).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  it('refuses an envelope it cannot walk', async () => {
    stub(200, { kind: 'success', value: { outcome: { outcome: 'evidence', value: {} } } });
    const result = await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views');
    expect(result).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  it('reports a version conflict as conflicting rather than as an error', async () => {
    // A product mutation preparation conflict says the graph head moved — a
    // reason to read again, not a transport fault to retry blindly.
    stub(409, { kind: 'problem', value: { problem: { kind: 'conflict' } } });
    const result = await callWork(
      WORK_PREPARE_GRAPH_MUTATION_ROUTE,
      PREPARE_REQUEST,
      '/api/work/prepare-graph-mutation',
    );
    expect(result).toMatchObject({ outcome: 'refused', state: 'conflicting' });
  });

  it('reports an unavailable Work runtime rather than an empty board', async () => {
    stub(503, { kind: 'problem', value: { problem: { kind: 'unavailable' } } });
    const result = await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views');
    expect(result).toMatchObject({ outcome: 'refused', state: 'unavailable' });
  });

  it('reports an unreachable daemon as offline', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('network');
      }),
    );
    const result = await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views');
    expect(result).toMatchObject({ outcome: 'refused', state: 'offline' });
  });

  it('reports a body that is not JSON without throwing', async () => {
    stub(200, null, { invalidJson: true });
    const result = await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views');
    expect(result).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  /** A command that does not satisfy its generated schema is refused here
   * rather than sent, so the user is told what is wrong instead of being handed
   * the daemon's 400. */
  it('refuses to send a command the generated contract rejects', async () => {
    const sent = vi.fn();
    vi.stubGlobal('fetch', sent);
    const result = await callWork(
      WORK_PREPARE_GRAPH_MUTATION_ROUTE,
      { change: { change: 'accept_task', task_id: 't1' } } as never,
      '/api/work/prepare-graph-mutation',
    );
    expect(result).toMatchObject({ outcome: 'refused', state: 'error' });
    expect(sent, 'an invalid command must not reach the daemon').not.toHaveBeenCalled();
  });

  it('POSTs the encoded command to the route it was given', async () => {
    const sent = vi.fn(async (_url: string, _init?: RequestInit) =>
      new Response(JSON.stringify(success(GRAPH)), { status: 200 }),
    );
    vi.stubGlobal('fetch', sent);
    await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/projects/p/work/views');
    expect(sent).toHaveBeenCalledWith('/api/projects/p/work/views', expect.objectContaining({ method: 'POST' }));
    const [, init] = sent.mock.calls[0] ?? [];
    expect(JSON.parse(String(init?.body))).toEqual(GRAPH_REQUEST);
  });
});
