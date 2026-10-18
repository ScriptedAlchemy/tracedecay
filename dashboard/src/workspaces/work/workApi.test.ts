import { afterEach, describe, expect, it, vi } from 'vitest';
import { z } from 'zod';

import type { EffectReceipt } from '../../contracts/generated.ts';
import {
  applicationProblem,
  applicationSuccess,
  FIXTURE_SCOPE,
} from '../../test/applicationEnvelope.ts';
import { responseWithBodyPendingUntilAbort } from '../../test/pendingBody.ts';
import { workGraphRead } from '../../test/workGraphFixture.ts';
import { callWork, workResultInvalidatesReads, type WorkResult } from './workApi.ts';
import { WORKFLOW_ACTIVATE_DEFINITION_ROUTE } from '../workflows/workflowRoutes.ts';
import {
  WORK_MUTATE_GRAPH_ROUTE,
  WORK_PREPARE_GRAPH_MUTATION_ROUTE,
  WORK_VIEWS_ROUTE,
} from './workRoutes.ts';

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

function views(): Promise<WorkResult<unknown>> {
  return callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views');
}

function prepare(): Promise<WorkResult<unknown>> {
  return callWork(
    WORK_PREPARE_GRAPH_MUTATION_ROUTE,
    PREPARE_REQUEST,
    '/api/work/prepare-graph-mutation',
  );
}

function successWith(patch: (value: Record<string, unknown>) => Record<string, unknown>) {
  const envelope = applicationSuccess('operation.work.views', GRAPH);
  return { ...envelope, value: patch(envelope.value) };
}

const COMMITTED_RECEIPT: EffectReceipt = {
  actor: 'actor.fixture',
  catalog_digest: 'sha256:catalog',
  committed_state: 'sha256:committed',
  configuration_digest: 'sha256:configuration',
  effect_class: 'administrative',
  expected_state: 'sha256:expected',
  external_proof: null,
  idempotency_key: 'idempotency.fixture',
  input_digest: 'sha256:input',
  operation: 'use_case.work.mutate_graph',
  outcome: 'partial',
  policy_digest: 'sha256:policy',
  privacy_digest: 'sha256:privacy',
  request_id: 'request.fixture',
  scope: FIXTURE_SCOPE,
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('a canonical success', () => {
  it('returns the payload decoded by its generated schema with the resolved scope', async () => {
    stub(200, applicationSuccess('operation.work.views', GRAPH));
    expect(await views()).toEqual({ outcome: 'value', value: GRAPH, scope: FIXTURE_SCOPE });
  });

  it('reads a durable command from its effect result', async () => {
    const route = { ...WORK_MUTATE_GRAPH_ROUTE, request: z.unknown(), response: z.unknown() };
    const receipt = { mutation: 'accept_task' };
    stub(200, applicationSuccess('operation.work.mutate_graph', receipt));
    expect(await callWork(route, {}, '/api/work/mutate-graph')).toMatchObject({
      outcome: 'value',
      value: receipt,
    });

    const asEvidence = applicationSuccess('operation.work.views', receipt);
    stub(200, {
      ...asEvidence,
      value: {
        ...asEvidence.value,
        binding_id: 'binding.http.work.mutate_graph',
        contract: { schema_id: 'schema.work.mutate_graph.result', schema_revision: 1 },
      },
    });
    expect(await callWork(route, {}, '/api/work/mutate-graph')).toMatchObject({
      outcome: 'refused',
      state: 'unsupported_schema',
    });
  });

  it.each([
    ['another binding', (value: Record<string, unknown>) => ({ ...value, binding_id: 'binding.http.work.topology' })],
    [
      'another result revision',
      (value: Record<string, unknown>) => ({
        ...value,
        contract: { schema_id: 'schema.work.views.result', schema_revision: 2 },
      }),
    ],
    [
      'another result contract',
      (value: Record<string, unknown>) => ({
        ...value,
        contract: { schema_id: 'schema.work.topology.result', schema_revision: 1 },
      }),
    ],
    [
      'an outcome its receipt contract does not admit',
      (value: Record<string, unknown>) => ({
        ...value,
        outcome: { ...(value.outcome as Record<string, unknown>), outcome: 'effect' },
      }),
    ],
    [
      'an evidence packet with no execution receipt',
      (value: Record<string, unknown>) => {
        const outcome = value.outcome as { outcome: string; value: Record<string, unknown> };
        const { execution: _execution, ...packet } = outcome.value;
        return { ...value, outcome: { ...outcome, value: packet } };
      },
    ],
  ])('refuses an envelope that answers %s', async (_name, patch) => {
    stub(200, successWith(patch));
    expect(await views()).toMatchObject({
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: 'the response envelope does not answer operation.work.views',
    });
  });

  it('refuses a success without a valid resolved scope', async () => {
    stub(200, successWith((value) => ({ ...value, scope: { project_id: 'project.work' } })));
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  it('refuses a payload the generated contract does not describe', async () => {
    stub(200, applicationSuccess('operation.work.views', { mode: 'current', snapshot: GRAPH.snapshot }));
    expect(await views()).toMatchObject({
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: 'the payload does not satisfy operation.work.views',
    });
  });

  it('refuses a payload of null as an answer without a value', async () => {
    stub(200, applicationSuccess('operation.work.views', null));
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unavailable' });
  });

  it('reads a lifecycle effect that timed out as timed out, and re-reads after it', async () => {
    const route = { ...WORKFLOW_ACTIVATE_DEFINITION_ROUTE, request: z.unknown() };
    const envelope = applicationSuccess('operation.workflow.activate_definition', null);
    const outcome = envelope.value.outcome as { value: { execution: Record<string, unknown> } };
    outcome.value.execution = {
      ...outcome.value.execution,
      termination: 'timed_out',
      cancellation: { stage: 'before_effect', observed_at: 2 },
    };
    stub(200, envelope);

    const result = await callWork(route, {}, '/api/application/workflow/activate-definition');

    expect(result).toMatchObject({ outcome: 'refused', state: 'timed_out', termination: 'timed_out' });
    expect(workResultInvalidatesReads(result)).toBe(true);
  });

  it.each([
    ['no deadline observation', null],
    ['no observation timestamp', { stage: 'before_effect' }],
    ['an unknown deadline stage', { stage: 'outside_the_contract', observed_at: 2 }],
  ])('refuses a timed-out lifecycle effect with %s', async (_name, cancellation) => {
    const route = { ...WORKFLOW_ACTIVATE_DEFINITION_ROUTE, request: z.unknown() };
    const envelope = applicationSuccess('operation.workflow.activate_definition', null);
    const outcome = envelope.value.outcome as { value: { execution: Record<string, unknown> } };
    outcome.value.execution = {
      ...outcome.value.execution,
      termination: 'timed_out',
      cancellation,
    };
    stub(200, envelope);
    expect(await callWork(route, {}, '/api/application/workflow/activate-definition')).toMatchObject({
      outcome: 'refused',
      state: 'unsupported_schema',
    });
  });

  it('refuses a problem served with a success status', async () => {
    stub(200, applicationProblem('unavailable', { binding: 'operation.work.views' }));
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  it('reports a body that is not JSON without throwing', async () => {
    stub(200, null, { invalidJson: true });
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });
});

describe('a canonical problem', () => {
  it('keeps stale and conflict apart and carries the retry and legal actions', async () => {
    stub(409, applicationProblem('stale', { binding: 'operation.work.prepare_graph_mutation' }));
    const stale = await prepare();
    stub(409, applicationProblem('conflict', { binding: 'operation.work.prepare_graph_mutation' }));
    const conflict = await prepare();

    expect(stale).toMatchObject({
      outcome: 'refused',
      state: 'stale',
      detail: 'fixture stale problem',
      problem: { kind: 'stale', retry: 'after_revalidate', legal_actions: ['refresh'] },
    });
    expect(conflict).toMatchObject({
      outcome: 'refused',
      state: 'conflicting',
      problem: { kind: 'conflict', retry: 'after_revalidate', legal_actions: ['refresh'] },
    });
  });

  it('carries the receipt of an effect that committed before the failure', async () => {
    stub(
      409,
      applicationProblem('partial_effect', {
        binding: 'operation.work.prepare_graph_mutation',
        problem: {
          committed_receipt: COMMITTED_RECEIPT,
          terminality: 'admitted_terminal',
          legal_actions: ['reconcile'],
        },
      }),
    );
    const result = await prepare();
    expect(result).toMatchObject({
      outcome: 'refused',
      state: 'partial',
      problem: { committed_receipt: COMMITTED_RECEIPT, legal_actions: ['reconcile'] },
    });
    expect(workResultInvalidatesReads(result)).toBe(true);
  });

  it('carries a reset directive', async () => {
    stub(
      503,
      applicationProblem('reset_required', {
        binding: 'operation.work.views',
        problem: { legal_actions: ['reset'], terminality: 'admitted_terminal' },
      }),
    );
    expect(await views()).toMatchObject({
      outcome: 'refused',
      state: 'unavailable',
      problem: { kind: 'reset_required', legal_actions: ['reset'] },
    });
  });

  it('reads a concealed denial, which names no binding, as denied', async () => {
    stub(404, applicationProblem('not_found_or_not_authorized'));
    expect(await views()).toMatchObject({
      outcome: 'refused',
      state: 'denied',
      problem: { kind: 'not_found_or_not_authorized' },
    });
  });

  it('refuses a concealed denial that names a binding', async () => {
    stub(404, applicationProblem('not_found_or_not_authorized', { binding: 'operation.work.views' }));
    expect(await views()).toMatchObject({
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: 'the problem does not answer operation.work.views',
    });
  });

  it('refuses a problem for another binding', async () => {
    stub(503, applicationProblem('unavailable', { binding: 'operation.work.topology' }));
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  it('reads a pre-admission problem, which names no binding', async () => {
    stub(503, applicationProblem('unavailable'));
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unavailable' });
  });

  it('refuses an admitted problem that names no route binding', async () => {
    stub(503, applicationProblem('reset_required'));
    expect(await views()).toMatchObject({
      outcome: 'refused',
      state: 'unsupported_schema',
      detail: 'the problem does not answer operation.work.views',
    });
  });

  it('refuses a problem record that does not satisfy the generated contract', async () => {
    stub(503, { kind: 'problem', value: { problem: { kind: 'unavailable' } } });
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  it('refuses a problem served with a status its kind does not map to', async () => {
    stub(503, applicationProblem('conflict', { binding: 'operation.work.views' }));
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });

  it('refuses a partial effect that carries no committed receipt', async () => {
    stub(409, applicationProblem('partial_effect', { binding: 'operation.work.prepare_graph_mutation' }));
    expect(await prepare()).toMatchObject({ outcome: 'refused', state: 'unsupported_schema' });
  });
});

describe('the dashboard gateway', () => {
  it('classifies authentication and origin failures without reading their bodies', async () => {
    stub(401, applicationProblem('unavailable'));
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'unauthorized' });
    stub(403, applicationProblem('unavailable'));
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'denied' });
  });

  it('reports a read-only project scope as locked with the gateway reason', async () => {
    stub(405, {
      status: 'read_only_project',
      detail: 'project-scoped dashboard APIs are read-only for non-active projects',
      project_id: 'project.other',
    });
    expect(await prepare()).toEqual({
      outcome: 'refused',
      state: 'locked',
      detail: 'project-scoped dashboard APIs are read-only for non-active projects',
    });
  });

  it('reports an unavailable selected project with the gateway reason', async () => {
    stub(503, { status: 'unavailable', detail: 'selected project Work authority is unavailable', project_id: 'p' });
    expect(await views()).toEqual({
      outcome: 'refused',
      state: 'unavailable',
      detail: 'selected project Work authority is unavailable',
    });
  });

  it('reports an unreachable daemon as offline', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('network');
      }),
    );
    expect(await views()).toMatchObject({ outcome: 'refused', state: 'offline' });
  });
});

describe('cancellation', () => {
  it('rejects when the caller aborts the request', async () => {
    const controller = new AbortController();
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init?: RequestInit) => {
        controller.abort();
        throw init?.signal?.reason;
      }),
    );
    await expect(
      callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views', controller.signal),
    ).rejects.toBe(controller.signal.reason);
  });

  it('rejects when the caller aborts while the body is being read', async () => {
    const controller = new AbortController();
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init?: RequestInit) =>
        responseWithBodyPendingUntilAbort(200, init?.signal),
      ),
    );
    const pending = callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/work/views', controller.signal);
    controller.abort();
    await expect(pending).rejects.toBe(controller.signal.reason);
  });
});

describe('requests', () => {
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

  it('POSTs the encoded command to the gateway path it was given', async () => {
    const sent = vi.fn(async (_url: string, _init?: RequestInit) =>
      new Response(JSON.stringify(applicationSuccess('operation.work.views', GRAPH)), { status: 200 }),
    );
    vi.stubGlobal('fetch', sent);
    await callWork(WORK_VIEWS_ROUTE, GRAPH_REQUEST, '/api/projects/p/work/views');
    expect(sent).toHaveBeenCalledWith('/api/projects/p/work/views', expect.objectContaining({ method: 'POST' }));
    const [, init] = sent.mock.calls[0] ?? [];
    expect(JSON.parse(String(init?.body))).toEqual(GRAPH_REQUEST);
  });
});

describe('workResultInvalidatesReads', () => {
  it('invalidates after a commit or a revalidation directive, never after a plain refusal', async () => {
    stub(200, applicationSuccess('operation.work.views', GRAPH));
    expect(workResultInvalidatesReads(await views())).toBe(true);
    stub(409, applicationProblem('conflict', { binding: 'operation.work.views' }));
    expect(workResultInvalidatesReads(await views())).toBe(true);
    stub(404, applicationProblem('not_found_or_not_authorized'));
    expect(workResultInvalidatesReads(await views())).toBe(false);
    stub(405, { status: 'read_only_project', detail: 'read-only', project_id: 'p' });
    expect(workResultInvalidatesReads(await views())).toBe(false);
  });
});
