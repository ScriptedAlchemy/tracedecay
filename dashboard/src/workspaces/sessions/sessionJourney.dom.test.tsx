import { MemoryRouter, Route, Routes, useLocation } from 'react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse, type JsonBodyType } from 'msw';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { fixtureServer } from '../../../stories/fixtures/handlers.ts';
import { useScope } from '../../data/scope/store.ts';
import { AgentsPage } from '../agents/AgentsPage.tsx';
import { LoomPage } from '../loom/LoomPage.tsx';
import { SessionsPage } from './SessionsPage.tsx';

const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledFrame: 'error' }));
beforeEach(() => {
  server.use(
    http.post('*/api/feedback/proximity', () =>
      HttpResponse.json(resolveFixture('/api/feedback/proximity') as JsonBodyType),
    ),
  );
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
  useScope.getState().selectProject('proj-1', 'Project one', 'active');
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  server.resetHandlers();
  useScope.getState().selectAllProjects();
});
afterAll(() => server.close());

const address = { pathname: '', search: '' };
function LocationProbe() {
  const location = useLocation();
  address.pathname = location.pathname;
  address.search = location.search;
  return null;
}

function renderJourney(entry: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route path="/sessions" element={<SessionsPage />} />
          <Route path="/agents" element={<AgentsPage />} />
          <Route path="/loom" element={<LoomPage />} />
        </Routes>
        <LocationProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function expectScoped() {
  const params = new URLSearchParams(address.search);
  expect(params.get('scope')).toBe('proj-1');
  expect(params.get('scopeLabel')).toBe('Project one');
}

const pivots = () => screen.getByRole('navigation', { name: 'Open this session in' });

describe('session investigation journey', () => {
  it('follows one session from Sessions to Agents to Loom and back, in scope', async () => {
    renderJourney('/sessions?scope=proj-1&scopeLabel=Project+one&sessionId=session.codex.child&sessionProvider=codex');
    const sessionsInspector = await screen.findByRole('complementary', { name: 'Inspector' });
    expect(within(sessionsInspector).getByText('session.codex.child')).toBeTruthy();

    await userEvent.click(within(pivots()).getByRole('link', { name: /Open in Agents/ }));
    expect(address.pathname).toBe('/agents');
    expectScoped();
    await waitFor(() =>
      expect(
        document.querySelector('[data-agent-inspector]')?.getAttribute('data-agent-inspector-id'),
      ).toBe('codex:session.codex.child'),
    );
    expect(document.querySelector('[data-agent-inspector]')?.getAttribute('data-agent-inspector-mode')).toBe(
      'selected',
    );

    await userEvent.click(within(pivots()).getByRole('link', { name: /Open in Loom/ }));
    expect(address.pathname).toBe('/loom');
    expectScoped();
    const thread = await screen.findByRole('region', { name: 'Loaded execution' });
    expect(thread.textContent).toContain('session.codex.child');
    expect(thread.textContent).toContain('codex');

    await userEvent.click(within(pivots()).getByRole('link', { name: /Open in Sessions/ }));
    expect(address.pathname).toBe('/sessions');
    expectScoped();
    expect(new URLSearchParams(address.search).get('sessionId')).toBe('session.codex.child');
    expect(new URLSearchParams(address.search).get('sessionProvider')).toBe('codex');
    const back = await screen.findByRole('complementary', { name: 'Inspector' });
    expect(within(back).getByText('session.codex.child')).toBeTruthy();
  }, 30_000);
});
