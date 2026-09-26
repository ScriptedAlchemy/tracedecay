import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { BrainPage } from './BrainPage.tsx';
import { useScope } from '../../data/scope/store.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';

vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: 'live', lastEventAt: null }),
  useLiveActivity: () => ({ pulses: [], revision: 0 }),
}));

const project = (id: string) => ({
  project_id: id,
  label: id,
  project_root: `/${id}`,
  canonical_root: `/${id}`,
  kind: 'primary',
  store_count: 1,
  artifact_count: 2,
  alias_count: 0,
  branches: [],
  default_branch: null,
  head_branch: null,
  last_seen_at: 1700000000,
});

function mount(count: number) {
  const groups = Array.from({ length: count }, (_, index) => {
    const id = `repo-${String(index).padStart(3, '0')}`;
    return { label: id, git_common_dir: `/${id}/.git`, branches: [], project_count: 1, projects: [project(id)] };
  });
  vi.stubGlobal(
    'fetch',
    vi.fn(async () =>
      new Response(
        JSON.stringify(
          fixtureEnvelope({
            status: 'ok',
            error: null,
            limit: 1000,
            truncated: false,
            active_project_id: null,
            active_project_root: '/',
            summary: { project_count: count, repo_count: count, truncated: false },
            project_tree: groups,
            projects: [],
          }),
        ),
      ),
    ),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <BrainPage />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.restoreAllMocks();
  useScope.getState().selectAllProjects();
  vi.unstubAllGlobals();
});

describe('Brain exact registry list', () => {
  it('windows a large registry instead of mounting a card per project', async () => {
    // jsdom has no layout; give every box a real height so the window has a size.
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue(DOMRect.fromRect({ width: 384, height: 80 }));
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(640);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(384);
    mount(150);
    const list = await screen.findByLabelText('150 registered projects');
    const mounted = within(list).queryAllByRole('button');
    expect(mounted.length).toBeGreaterThan(0);
    expect(mounted.length).toBeLessThan(150);
    expect(mounted[0]!.textContent).toContain('repo-000');
  });

  it('keeps every match as plain cards once search narrows below the window threshold', async () => {
    mount(150);
    await screen.findByLabelText('150 registered projects');
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search project registry' }), { target: { value: 'repo-00' } });
    expect(screen.getByText('10 matching projects')).toBeTruthy();
    expect(screen.queryByLabelText(/registered projects$/)).toBeNull();
    const registry = screen.getByRole('region', { name: 'Project registry' });
    expect(within(registry).getAllByRole('button', { name: /^repo-00\d/ })).toHaveLength(10);
  });
});
