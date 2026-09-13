import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createMemoryRouter, RouterProvider, useLocation } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { NavRail } from '../../app/shell/NavRail.tsx';
import { ScopeUrlSync } from './UrlSync.tsx';
import { useScope } from './store.ts';

function LocationProbe() {
  const location = useLocation();
  return <output aria-label="Current location">{location.pathname + location.search}</output>;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

describe('project scope workspace navigation', () => {
  it('carries a deep-linked project across the rail and restores the source on Back', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    const router = createMemoryRouter(
      [
        {
          path: '*',
          element: (
            <>
              <ScopeUrlSync />
              <NavRail />
              <LocationProbe />
            </>
          ),
        },
      ],
      {
        initialEntries: ['/brain?scope=proj_td&scopeLabel=tracedecay&brainView=registry'],
      },
    );
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={client}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );

    await waitFor(() =>
      expect(useScope.getState().scope).toMatchObject({
        kind: 'project',
        projectId: 'proj_td',
      }),
    );

    await userEvent.click(screen.getByRole('link', { name: 'Code' }));
    await waitFor(() =>
      expect(screen.getByLabelText('Current location').textContent).toBe(
        '/code?scope=proj_td&scopeLabel=tracedecay',
      ),
    );
    expect(useScope.getState().scope).toMatchObject({
      kind: 'project',
      projectId: 'proj_td',
    });

    await act(async () => {
      await router.navigate(-1);
    });
    await waitFor(() =>
      expect(screen.getByLabelText('Current location').textContent).toBe(
        '/brain?scope=proj_td&scopeLabel=tracedecay&brainView=registry',
      ),
    );
    expect(useScope.getState().scope).toMatchObject({
      kind: 'project',
      projectId: 'proj_td',
    });
  });
});
