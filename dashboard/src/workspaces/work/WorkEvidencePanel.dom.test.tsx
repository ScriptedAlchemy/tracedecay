import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { WorkGraphReadV1Schema, type WorkGraphReadV1 } from '../../contracts/index.ts';
import { workGraphRead } from '../../test/workGraphFixture.ts';
import type { WorkResult } from './workApi.ts';
import { WorkEvidencePanel } from './WorkEvidencePanel.tsx';

const VERSION = {
  event_sequence: 12,
  graph_version: 7,
  recovered_graph_digest: 'digest-graph',
  source_watermark: {},
};
const ATTEMPT = { task_id: 'task.alpha', run_id: 'run.1', attempt_id: 'attempt.1' };
const CONTINUATION = {
  kind: 'task_session' as const,
  continuation: {
    attempt: ATTEMPT,
    participant_epoch: 'digest.participants',
    ranking_cursor: 'ranking.cursor',
    source: { provider: 'codex', session_id: 'session.1' },
    temporal_cursor: 'temporal.cursor',
    verified_version: VERSION,
  },
};

const graph: WorkResult<WorkGraphReadV1> = {
  outcome: 'value',
  value: WorkGraphReadV1Schema.parse(
    workGraphRead({ tasks: [{ taskId: 'task.alpha' }], version: VERSION.graph_version }),
  ),
};

function payload(withContinuation: boolean) {
  return {
    task_id: 'task.alpha',
    verified_version: VERSION,
    item: {
      accepted_at: null,
      accepted_attempts: [ATTEMPT],
      accepted_criteria: {},
      accepted_proposal: null,
      accepted_route: null,
      archived_at: null,
      evidence_links: [],
      execution_admitted_at: 100,
      handoffs: [],
      input: {
        acceptance_criteria: [],
        causal_candidates: [],
        created_at: 1,
        deadline: null,
        dependencies: [],
        effort: 1,
        hierarchy: {
          initiative_id: 'initiative.1',
          milestone_id: 'milestone.1',
          plan_id: 'plan.1',
        },
        informational_relations: [],
        scheduled_at: null,
        task_id: 'task.alpha',
        title: 'Alpha',
        updated_at: 2,
      },
    },
    relations: [],
    proposal_decisions: [],
    relation_replan_decisions: [],
    sources: [
      {
        kind: 'task_session',
        attempt: ATTEMPT,
        evidence: {
          task_id: 'task.alpha',
          verified_version: VERSION,
          attempt: ATTEMPT,
          source: { provider: 'codex', session_id: 'session.1' },
          participant_epoch: 'digest.participants',
          ranked_anchors: [],
          hydrated: [
            {
              rank: 0,
              anchor_id: 'anchor.1',
              state: 'available',
              content: Array.from(new TextEncoder().encode('Provider completed the task')),
            },
          ],
          coverage: withContinuation ? 'partial' : 'complete',
          coverage_counts: { visible: 1, hidden: 0, unknown: 0, redacted: 0 },
          freshness: 'current',
          redacted: false,
          continuation: withContinuation ? CONTINUATION.continuation : null,
        },
      },
    ],
    coverage: {
      state: withContinuation ? 'partial' : 'complete',
      selected: 1,
      hydrated: 1,
      omitted: 0,
    },
    omissions: [],
    freshness: 'current',
    redacted: false,
    continuations: withContinuation ? [CONTINUATION] : [],
  };
}

function envelope(value: unknown) {
  return {
    kind: 'success',
    value: {
      outcome: { outcome: 'evidence', value: { payload: value } },
    },
  };
}

afterEach(() => vi.unstubAllGlobals());

describe('selected task evidence', () => {
  it('shows provider-qualified content and continues only with the sealed relation', async () => {
    const requests: unknown[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init?: RequestInit) => {
        requests.push(JSON.parse(String(init?.body)));
        return new Response(JSON.stringify(envelope(payload(requests.length === 1))), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      }),
    );
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <WorkEvidencePanel taskId="task.alpha" graph={graph} />
        </MemoryRouter>
      </QueryClientProvider>,
    );

    expect((await screen.findAllByText('codex / session.1')).length).toBeGreaterThan(0);
    expect(screen.getByText('Provider completed the task')).toBeTruthy();
    expect(requests[0]).toMatchObject({
      task_id: 'task.alpha',
      verified_version: VERSION,
      temporal: { kind: 'forensic' },
      expansion: null,
      continuation: null,
    });

    await userEvent.click(screen.getByRole('button', { name: 'Continue provider session' }));
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1]).toMatchObject({
      expansion: { kind: 'task_session', attempt: ATTEMPT },
      continuation: CONTINUATION,
    });
  });
});
