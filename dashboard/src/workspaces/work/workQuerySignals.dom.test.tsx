import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { WorkGraphReadV1Schema } from '../../contracts/generated.ts';
import { useFeedbackProximity } from '../../viz/proximity/proximityApi.ts';
import { responseWithBodyPendingUntilAbort } from '../../test/pendingBody.ts';
import { workGraphRead } from '../../test/workGraphFixture.ts';
import { useAgentHandoffTokens } from '../agents/handoffTokenQuery.ts';
import { useAgentWorkGraph } from '../agents/agentWorkQuery.ts';
import {
  useWorkflowDefinitionHistory,
  useWorkflowDefinitions,
  useWorkflowRun,
} from '../workflows/workflowQueries.ts';
import { useWorkEvidence } from './workEvidenceQueries.ts';
import {
  useWorkAttempts,
  useWorkExecutionHistory,
  useWorkGraphViews,
  useWorkTopology,
  useWorkTopologyMetrics,
} from './workViewsQueries.ts';

const GRAPH = {
  outcome: 'value' as const,
  value: WorkGraphReadV1Schema.parse(
    workGraphRead({ tasks: [{ taskId: 'task-alpha', title: 'Alpha task' }] }),
  ),
};

const QUERIES: ReadonlyArray<readonly [string, () => { data: unknown }]> = [
  ['work views', () => useWorkGraphViews(true)],
  ['work attempts', () => useWorkAttempts(true)],
  ['work execution history', () => useWorkExecutionHistory(true)],
  ['work topology', () => useWorkTopology(true)],
  ['work topology metrics', () => useWorkTopologyMetrics(true)],
  ['work evidence', () => useWorkEvidence(GRAPH, 'task-alpha', { kind: 'current' }, null)],
  ['agents work graph', () => useAgentWorkGraph()],
  ['agents handoff tokens', () => useAgentHandoffTokens('session-1')],
  ['feedback proximity', () => useFeedbackProximity(true)],
  ['workflow definitions', () => useWorkflowDefinitions()],
  ['workflow definition history', () => useWorkflowDefinitionHistory('workflow.one')],
  ['workflow run', () => useWorkflowRun('run.one')],
];

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('application-route query cancellation', () => {
  it.each(QUERIES)('%s aborts its request and caches nothing', async (_name, useQueryUnderTest) => {
    const signals: AbortSignal[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init?: RequestInit) => {
        if (init?.signal == null) throw new Error('the query sent no abort signal');
        signals.push(init.signal);
        return responseWithBodyPendingUntilAbort(200, init.signal);
      }),
    );
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    );
    const { result } = renderHook(useQueryUnderTest, { wrapper });

    await waitFor(() => expect(signals).toHaveLength(1));
    await client.cancelQueries();

    expect(signals[0]?.aborted, 'the request was not aborted').toBe(true);
    expect(result.current.data, 'a cancelled read left a cached result').toBeUndefined();
  });
});
