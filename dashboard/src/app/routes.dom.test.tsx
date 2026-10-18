/**
 * Issue #4220: fourteen workspaces became five task families. Every path that
 * existed before still has to land on the equivalent view, with the project
 * scope and the view's own parameters intact, and every view has to be reachable
 * from its family's tab strip.
 *
 * The page chunks are stubbed so this file measures routing alone; the task
 * tests in `tasks.dom.test.tsx` mount the real pages.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createMemoryRouter, RouterProvider } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../data/scope/store.ts';
import { CHANNELS } from './channels.ts';
import { LEGACY_PATHS } from './legacyRoutes.ts';
import { dashboardRoutes } from './routes.tsx';

vi.mock('../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: 'connecting' as const, lastEventAt: null }),
  useEventsConnection: () => null,
  useProjectionSync: () => ({ kind: 'unmounted' }) as const,
}));

vi.mock('./viewChunks.ts', async () => {
  const { CHANNELS: channels } = await import('./channels.ts');
  const chunks = Object.fromEntries(
    channels.flatMap((channel) =>
      channel.views.map((entry) => [
        entry.path,
        async () => ({
          default: () => <div data-testid="view" data-view={entry.path} />,
        }),
      ]),
    ),
  );
  return { VIEW_CHUNKS: chunks };
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

async function open(entry: string) {
  vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
  const router = createMemoryRouter(dashboardRoutes(), { initialEntries: [entry] });
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  await screen.findByTestId('view');
  return router;
}

const where = (router: Awaited<ReturnType<typeof open>>) => {
  const { pathname, search, hash } = router.state.location;
  return { pathname, params: Object.fromEntries(new URLSearchParams(search)), hash };
};

const SCOPE = 'scope=prj_1&scopeLabel=Demo';

describe('retired paths', () => {
  it.each([
    ['/brain', '/memory'],
    ['/explorer', '/memory/explorer'],
    ['/knowledge', '/memory/knowledge'],
    ['/sessions', '/memory/sessions'],
    ['/loom', '/memory/loom'],
    ['/agents', '/memory/agents'],
    ['/delivery', '/code/delivery'],
    ['/workflows', '/work/workflows'],
    ['/observatory', '/operations'],
    ['/automations', '/operations/automations'],
    ['/costs', '/operations/costs'],
    ['/', '/memory'],
  ])('%s opens %s with its scope, parameters and hash intact', async (from, to) => {
    const router = await open(`${from}?${SCOPE}&q=needle&sessionId=s1#frag`);

    expect(where(router)).toEqual({
      pathname: to,
      params: { scope: 'prj_1', scopeLabel: 'Demo', q: 'needle', sessionId: 's1' },
      hash: '#frag',
    });
    expect(screen.getByTestId('view').getAttribute('data-view')).toBe(to.slice(1));
  });

  it('covers every retired path the legacy table names', () => {
    expect(Object.keys(LEGACY_PATHS).sort()).toEqual([
      'agents',
      'automations',
      'brain',
      'costs',
      'delivery',
      'explorer',
      'knowledge',
      'loom',
      'observatory',
      'sessions',
      'workflows',
    ]);
  });

  it('leaves the three paths that did not move where they were', async () => {
    for (const path of ['/code', '/work', '/settings']) {
      const router = await open(`${path}?${SCOPE}`);
      expect(where(router).pathname).toBe(path);
      cleanup();
    }
  });

  it('opens Loom for the removed Agents timeline view, carrying the linked session', async () => {
    const router = await open(
      `/agents?view=timeline&${SCOPE}&sessionId=session.codex.child&sessionProvider=codex`,
    );

    expect(where(router)).toEqual({
      pathname: '/memory/loom',
      params: {
        scope: 'prj_1',
        scopeLabel: 'Demo',
        loomSession: '["codex","session.codex.child"]',
      },
      hash: '',
    });
  });

  it('keeps a provider-less session as a bare id for Loom to resolve', async () => {
    const router = await open('/agents?view=timeline&sessionId=session.bare');

    expect(where(router).params).toEqual({ loomSession: 'session.bare' });
  });

  it('keeps the topology view of Agents on Agents', async () => {
    const router = await open('/agents?sessionId=session.codex.child&sessionProvider=codex');

    expect(where(router)).toMatchObject({
      pathname: '/memory/agents',
      params: { sessionId: 'session.codex.child', sessionProvider: 'codex' },
    });
  });
});

describe('the family tab strip', () => {
  it.each(CHANNELS.filter((channel) => channel.views.length > 1))(
    '$label draws every view as a visible link and reaches each one',
    async (channel) => {
      const user = userEvent.setup();
      const router = await open(`/${channel.path}?${SCOPE}`);
      const strip = screen.getByRole('navigation', { name: `${channel.label} views` });

      expect(within(strip).getAllByRole('link').map((link) => link.textContent)).toEqual(
        channel.views.map((entry) => entry.label),
      );
      expect(
        within(strip).getAllByRole('link').filter((link) => link.getAttribute('aria-current') === 'page'),
      ).toHaveLength(1);

      for (const entry of channel.views) {
        await user.click(within(strip).getByRole('link', { name: entry.label }));
        await waitFor(() => expect(where(router).pathname).toBe(`/${entry.path}`));
        // The project scope rides along on every hop.
        expect(where(router).params).toMatchObject({ scope: 'prj_1', scopeLabel: 'Demo' });
        await waitFor(() =>
          expect(screen.getByTestId('view').getAttribute('data-view')).toBe(entry.path),
        );
        expect(
          within(strip).getByRole('link', { name: entry.label }).getAttribute('aria-current'),
        ).toBe('page');
      }
    },
  );

  it('draws no tab strip for a family of one view', async () => {
    await open('/settings');

    expect(screen.queryByRole('navigation', { name: 'Settings views' })).toBeNull();
  });
});
