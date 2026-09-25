import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { http, HttpResponse, type JsonBodyType } from 'msw';
import { MemoryRouter } from 'react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { fixtureServer } from '../../../stories/fixtures/handlers.ts';
import { AgentsPage } from './AgentsPage.tsx';

/**
 * Per-session provider usage on both views. The fixture tree holds all three
 * node states: measured (root, orphan), partial (child: `complete: false`),
 * and absent (grandchild, solo), under a tree coverage of `partial`.
 */
const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

async function renderAgents(entry = '/agents') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <AgentsPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  await screen.findByText(/^\d+ sessions · \d+ drawn · \d+ generations$/);
}

const control = (id: string) =>
  document.querySelector<HTMLElement>(`[data-topology-control="session"][data-topology-id="${id}"]`)!;

/** Every session control's usage label, as rendered, in tab order. */
function usageLabels(scope: string) {
  return [...document.querySelectorAll(`${scope} [data-topology-control="session"]`)].map((node) => {
    const label = node.querySelector('[data-usage-state]')!;
    return [
      node.getAttribute('data-topology-id'),
      label.getAttribute('data-usage-state'),
      label.querySelector('[data-usage-total]')!.textContent,
      label.querySelector('[data-usage-partial]')?.textContent ?? null,
    ];
  });
}

const inspectorUsage = () => document.querySelector('[data-agent-inspector-usage]')!;

describe('agents per-session provider usage', () => {
  it('rings print each total with its unit, the partial marker, or absent', async () => {
    await renderAgents();
    expect(usageLabels('[data-delegation-topology]')).toEqual([
      ['codex:session.codex.root', 'measured', '1,515,630 tokens', null],
      ['claude:session.claude.orphan', 'measured', '108,400 tokens', null],
      ['cursor:session.cursor.solo', 'absent', 'tokens absent', null],
      ['codex:session.codex.child', 'partial', '357,250 tokens', 'partial'],
      ['codex:session.codex.grandchild', 'absent', 'tokens absent', null],
    ]);
    const legend = document.querySelector('[data-usage-coverage]')!;
    expect(legend.getAttribute('data-usage-coverage')).toBe('partial');
    expect(legend.textContent).toBe('partialtokens · usage read partial · absent may be unread');
  });

  it('the inspector splits a measured session by counter', async () => {
    await renderAgents();
    fireEvent.click(control('codex:session.codex.root'));
    await waitFor(() => expect(inspectorUsage().getAttribute('data-agent-inspector-usage')).toBe('measured'));
    expect(inspectorUsage().textContent).toBe(
      'total1,515,630 tokensinput182,400 tokensoutput24,310 tokenscache read1,204,800 tokens' +
        'cache write96,000 tokensreasoning8,120 tokensevents64',
    );
  });

  it('a partial aggregate keeps its counters, marks unreported ones, and says it is a floor', async () => {
    await renderAgents();
    fireEvent.click(control('codex:session.codex.child'));
    await waitFor(() => expect(inspectorUsage().getAttribute('data-agent-inspector-usage')).toBe('partial'));
    expect(inspectorUsage().textContent).toBe(
      'total357,250 tokensinput41,200 tokensoutput6,050 tokenscache read310,000 tokens' +
        'cache writeunreportedreasoningunreportedevents11',
    );
    const section = screen.getByRole('region', { name: 'Provider usage' });
    expect(section.querySelector('[data-usage-partial]')!.textContent).toBe('partial');
    expect(section.textContent).toContain('the provider marked this aggregate incomplete · counts are a floor');
  });

  it('an absent session reads against the tree coverage', async () => {
    await renderAgents();
    fireEvent.click(control('codex:session.codex.grandchild'));
    await waitFor(() => expect(inspectorUsage().getAttribute('data-agent-inspector-usage')).toBe('absent'));
    expect(inspectorUsage().textContent).toBe('tokens absent · usage read partial · absent may be unread');
  });

  it('the timeline rows carry the same labels', async () => {
    await renderAgents('/agents?view=timeline');
    expect(usageLabels('[data-delegation-timeline]').map(([id, state, total]) => [id, state, total])).toEqual([
      ['codex:session.codex.root', 'measured', '1,515,630 tokens'],
      ['codex:session.codex.child', 'partial', '357,250 tokens'],
      ['codex:session.codex.grandchild', 'absent', 'tokens absent'],
      ['claude:session.claude.orphan', 'measured', '108,400 tokens'],
      ['cursor:session.cursor.solo', 'absent', 'tokens absent'],
    ]);
    expect(document.querySelector('[data-usage-coverage]')!.getAttribute('data-usage-coverage')).toBe('partial');
  });

  it('a failed usage read is distinguishable from none recorded', async () => {
    const served = resolveFixture('/api/plugins/analytics/subagent-tree') as {
      payload: { nodes: Record<string, unknown>[] };
    };
    const payload = {
      ...served.payload,
      usage_coverage: 'unavailable',
      nodes: served.payload.nodes.map(({ usage: _usage, ...node }) => node),
    };
    server.use(
      http.get('*/api/plugins/analytics/subagent-tree', () =>
        HttpResponse.json({ ...served, payload } as JsonBodyType),
      ),
    );
    await renderAgents();
    expect(new Set(usageLabels('[data-delegation-topology]').map(([, state, total]) => `${state}:${total}`))).toEqual(
      new Set(['absent:tokens absent']),
    );
    expect(document.querySelector('[data-usage-coverage]')!.textContent).toBe(
      'tokens · usage read unavailable · every session absent',
    );
    fireEvent.click(control('codex:session.codex.root'));
    await waitFor(() =>
      expect(inspectorUsage().textContent).toBe('tokens absent · usage read unavailable · every session absent'),
    );
  });
});
