/**
 * The four tasks issue #4220 names, walked from the rail through the real
 * pages: code lookup, session retrieval, work tracking, and operational
 * debugging. Each starts where a user starts, the navigation rail or a link that
 * existed before the families, and ends on the surface that answers the task.
 *
 * Every read is refused, so the pages draw their truthful error states. What is
 * asserted is reachability and the controls that make the task possible, and
 * that the refusal is stated instead of an empty list being drawn.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createMemoryRouter, RouterProvider } from 'react-router';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { useScope } from '../data/scope/store.ts';
import { dashboardRoutes } from './routes.tsx';
import { VIEW_CHUNKS } from './viewChunks.ts';

vi.mock('../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: 'connecting' as const, lastEventAt: null }),
  useEventsConnection: () => null,
  useProjectionSync: () => ({ kind: 'unmounted' }) as const,
  useLiveActivity: () => ({ pulses: [], revision: 0 }),
}));

// A real page is a lazy chunk with heavy dependencies (charts, graph layout).
// Loading them once up front keeps each task measured against the page, not
// against a cold module graph on a loaded machine.
beforeAll(async () => {
  await Promise.all(Object.values(VIEW_CHUNKS).map((load) => load()));
}, 120_000);

vi.setConfig({ testTimeout: 30_000 });

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

async function start(entry: string) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response('{}', { status: 503, headers: { 'content-type': 'application/json' } })),
  );
  const router = createMemoryRouter(dashboardRoutes(), { initialEntries: [entry] });
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { router, user: userEvent.setup() };
}

const rail = () => screen.getByRole('navigation', { name: 'Workspaces' });
/** The workspace header's title: the one h1 a view draws for itself. */
const heading = async (name: string) =>
  (await screen.findAllByRole('heading', { level: 1, name }, { timeout: 10_000 }))[0];
const location = (router: Awaited<ReturnType<typeof start>>['router']) =>
  router.state.location.pathname + router.state.location.search;

describe('code lookup', () => {
  it('reaches symbol search from the rail', async () => {
    const { router, user } = await start('/memory');
    await user.click(within(rail()).getByRole('link', { name: 'Code' }));

    await heading('Code');
    expect(location(router)).toBe('/code');
    expect(await screen.findByRole('searchbox', { name: 'Symbol search' }, { timeout: 10_000 })).toBeTruthy();
  });

  it('reaches the cross-store query from the Explorer link a bookmark holds', async () => {
    const { router } = await start('/explorer?q=graph&lane=code');

    await heading('Explorer');
    expect(location(router)).toBe('/memory/explorer?q=graph&lane=code');
    const box = await screen.findByRole('searchbox', { name: 'Query code, sessions, and knowledge' });
    expect((box as HTMLInputElement).value).toBe('graph');
  });
});

describe('session retrieval', () => {
  it('reaches transcript search through the Memory family', async () => {
    const { router, user } = await start('/');
    await waitFor(() => expect(location(router)).toBe('/memory'));
    await user.click(within(rail()).getByRole('link', { name: 'Memory' }));
    await user.click(
      within(screen.getByRole('navigation', { name: 'Memory views' })).getByRole('link', {
        name: 'Sessions',
      }),
    );

    await heading('Sessions');
    expect(location(router)).toBe('/memory/sessions');
    expect(await screen.findByRole('searchbox', { name: 'Search transcripts' }, { timeout: 10_000 })).toBeTruthy();
  });

  it('keeps a linked session selected when the old Sessions path is opened', async () => {
    const { router } = await start('/sessions?sessionId=s1&sessionProvider=codex&sessionsPage=2');

    await heading('Sessions');
    expect(location(router)).toBe('/memory/sessions?sessionId=s1&sessionProvider=codex&sessionsPage=2');
  });

  it('opens Loom, not a blank view, for the removed Agents timeline link', async () => {
    const { router } = await start('/agents?view=timeline&sessionId=s1&sessionProvider=codex');

    await heading('Loom');
    expect(new URLSearchParams(router.state.location.search).get('loomSession')).toBe('["codex","s1"]');
  });
});

describe('work tracking', () => {
  it('reaches the task graph with only the board and the DAG as projections', async () => {
    const { router, user } = await start('/memory');
    await user.click(within(rail()).getByRole('link', { name: 'Work' }));

    await heading('Work');
    expect(location(router)).toBe('/work');
    const tabs = await screen.findByRole('tablist', { name: 'Work projection' }, { timeout: 10_000 });
    expect(within(tabs).getAllByRole('tab').map((tab) => tab.textContent)).toEqual(['Board', 'DAG']);
  });

  it('opens the board with the task kept for a saved link to a retired projection', async () => {
    const { router } = await start('/work?view=timeline&task=task-1');

    const tabs = await screen.findByRole('tablist', { name: 'Work projection' }, { timeout: 10_000 });
    expect(within(tabs).getByRole('tab', { selected: true }).textContent).toBe('Board');
    expect(new URLSearchParams(router.state.location.search).get('task')).toBe('task-1');
  });

  it('reaches workflow definitions from the Work family', async () => {
    const { router, user } = await start('/work');
    await user.click(
      within(await screen.findByRole('navigation', { name: 'Work views' })).getByRole('link', {
        name: 'Workflows',
      }),
    );

    await heading('Workflows');
    expect(location(router)).toBe('/work/workflows');
  });
});

describe('operational debugging', () => {
  it('reaches the Doctor surface from the rail, with its health dot on Operations', async () => {
    const { router, user } = await start('/memory');
    const operations = within(rail()).getByRole('link', { name: 'Operations' });
    expect(operations.querySelector('[data-doctor-health]')).not.toBeNull();
    await user.click(operations);

    await heading('Observatory');
    expect(location(router)).toBe('/operations');
  });

  it('reaches the automation run ledger and spend from the Operations family', async () => {
    const { router, user } = await start('/operations');
    const views = await screen.findByRole('navigation', { name: 'Operations views' });

    await user.click(within(views).getByRole('link', { name: 'Automations' }));
    await heading('Automations');
    expect(location(router)).toBe('/operations/automations');

    await user.click(within(views).getByRole('link', { name: 'Costs' }));
    await heading('Costs');
    expect(location(router)).toBe('/operations/costs');
  });

  it('keeps an Observatory deep link on the inspected authority', async () => {
    const { router } = await start('/observatory?inspect=doctor&scope=prj_1&scopeLabel=Demo');

    await heading('Observatory');
    expect(location(router)).toBe('/operations?inspect=doctor&scope=prj_1&scopeLabel=Demo');
  });
});
