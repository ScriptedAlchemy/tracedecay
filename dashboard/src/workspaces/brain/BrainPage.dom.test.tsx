import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { BrainPage } from './BrainPage.tsx';
import { useScope } from '../../data/scope/store.ts';
import { ProjectsPayloadSchema } from './contracts.ts';

function registryBody(status: 'ok' | 'missing_registry' | 'registry_unavailable') {
  return {
    status,
    summary: { project_count: 0, repo_count: 0, truncated: false },
    project_tree: [],
  };
}

function renderBrain(status: 'ok' | 'missing_registry' | 'registry_unavailable') {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(registryBody(status)), { status: 200 })),
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <BrainPage />
    </QueryClientProvider>,
  );
}

describe('BrainPage registry states', () => {
  afterEach(() => {
    useScope.getState().selectAllProjects();
    vi.unstubAllGlobals();
  });

  it('does not render a registry query failure as zero projects', async () => {
    renderBrain('registry_unavailable');

    expect(await screen.findByText(/registry read failed/i)).toBeTruthy();
    expect(screen.queryByText(/0 repositories · 0 projects/i)).toBeNull();
  });

  it('rejects unknown registry statuses at the parser boundary', () => {
    expect(
      ProjectsPayloadSchema.safeParse({
        ...registryBody('ok'),
        status: 'mystery_success',
      }).success,
    ).toBe(false);
  });

  it('separates a missing registry from a successful empty registry', async () => {
    const missing = renderBrain('missing_registry');
    expect(await screen.findByText(/registry is not configured/i)).toBeTruthy();
    expect(screen.queryByText(/contains no projects/i)).toBeNull();
    missing.unmount();

    renderBrain('ok');
    expect(await screen.findByText(/registry contains no projects/i)).toBeTruthy();
    expect(screen.queryByText(/registry is not configured/i)).toBeNull();
  });
});
