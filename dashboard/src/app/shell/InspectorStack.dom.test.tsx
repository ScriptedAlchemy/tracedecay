import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { createMemoryRouter, RouterProvider } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../data/scope/store.ts';
import { Shell } from './Shell.tsx';

vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: 'connecting', lastEventAt: null }),
  useProjectionSync: () => ({ kind: 'unmounted' }) as const,
}));

afterEach(() => {
  useScope.getState().selectAllProjects();
});

function encodedInspector(
  entityId: string,
  evidenceId: string,
  projectId = 'project-alpha',
): string {
  return JSON.stringify({
    scope: { kind: 'project', project_id: projectId },
    entity: { kind: 'task', id: entityId },
    evidence: { kind: 'attempt', id: evidenceId },
  });
}

function dashboardUrl(entries: readonly [entityId: string, evidenceId: string][]): string {
  const params = new URLSearchParams();
  params.set('scope', 'project-alpha');
  params.set('scopeLabel', 'Alpha');
  for (const [entityId, evidenceId] of entries) {
    params.append('inspect', encodedInspector(entityId, evidenceId));
  }
  return `/work?${params.toString()}`;
}

function mount(initialEntry: string) {
  const router = createMemoryRouter(
    [
      {
        path: '/',
        element: <Shell />,
        children: [{ path: 'work', element: <p>Work surface</p> }],
      },
    ],
    { initialEntries: [initialEntry] },
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

describe('universal inspector stack', () => {
  it('restores a bounded stack with exact scope, entity, and evidence identities', async () => {
    mount(
      dashboardUrl([
        ['task-a', 'attempt-a'],
        ['task-b', 'attempt-b'],
        ['task-c', 'attempt-c'],
        ['task-d', 'attempt-d'],
        ['task-e', 'attempt-e'],
      ]),
    );

    const inspector = await screen.findByRole('complementary', {
      name: 'Inspector stack',
    });
    expect(within(inspector).queryByRole('tab', { name: /task-a/ })).toBeNull();
    expect(within(inspector).getAllByRole('tab')).toHaveLength(4);
    expect(within(inspector).getByRole('tab', { name: /task-e/ }).getAttribute('aria-selected'))
      .toBe('true');
    const activePanel = within(inspector).getByRole('tabpanel');
    expect(within(activePanel).getByText('project-alpha')).toBeTruthy();
    expect(within(activePanel).getByText('task-e')).toBeTruthy();
    expect(within(activePanel).getByText('attempt-e')).toBeTruthy();
  });

  it('closes and reorders by stable identity without retargeting the active entry', async () => {
    mount(
      dashboardUrl([
        ['task-a', 'attempt-a'],
        ['task-b', 'attempt-b'],
        ['task-c', 'attempt-c'],
      ]),
    );

    const inspector = await screen.findByRole('complementary', {
      name: 'Inspector stack',
    });
    expect(within(inspector).getByRole('tab', { name: /task-c/ }).getAttribute('aria-selected'))
      .toBe('true');

    fireEvent.click(within(inspector).getByRole('button', { name: 'Close task-b' }));
    expect(within(inspector).queryByRole('tab', { name: /task-b/ })).toBeNull();
    expect(within(inspector).getByRole('tab', { name: /task-c/ }).getAttribute('aria-selected'))
      .toBe('true');
    expect(within(inspector).getByText('attempt-c')).toBeTruthy();

    fireEvent.click(within(inspector).getByRole('button', { name: 'Move task-a later' }));
    expect(within(inspector).getByRole('tab', { name: /task-c/ }).getAttribute('aria-selected'))
      .toBe('true');
    expect(within(inspector).getByText('attempt-c')).toBeTruthy();
  });
});
