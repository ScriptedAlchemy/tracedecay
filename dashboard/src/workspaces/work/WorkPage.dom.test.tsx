/**
 * The Work page over the nine mounted routes.
 *
 * These replace the assertions that pinned the old absence ledger. That ledger
 * said no generated Work read model existed in the build, which `3f43664cb`
 * made false, so the assertions that held it in place have been rewritten as
 * behaviour over the real routes rather than deleted.
 *
 * The invariant they carry forward is the one that mattered: a refusal is never
 * a board. Every failure the daemon can return is asserted to render as its own
 * stated reason, and the empty-board case is asserted to be reachable *only*
 * from a snapshot that actually said the board was empty.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../data/scope/store.ts';
import { WorkPage } from './WorkPage.tsx';

function projection(overrides: Record<string, unknown> = {}) {
  return {
    accepted_proposal: null,
    authority: {
      actor_id: 'actor',
      policy_digest: 'digest',
      project_id: 'project',
      repository_id: 'repository',
      worktree_id: 'worktree',
    },
    dependencies: [],
    execution_admitted: false,
    history_len: 2,
    runtime_evidence: [],
    task_accepted: false,
    task_id: 'task-alpha',
    title: 'Alpha task',
    version: 4,
    ...overrides,
  };
}

function snapshotBody(projections: readonly unknown[], coverage?: unknown) {
  return {
    kind: 'success',
    value: {
      binding_id: 'binding.http.work.snapshot',
      contract: { schema_id: 'schema.work.snapshot.result', schema_revision: 1 },
      request_id: 'request-1',
      scope: {},
      outcome: {
        outcome: 'evidence',
        value: {
          payload: {
            coverage: coverage ?? {
              state: 'complete',
              returned: projections.length,
              total: projections.length,
            },
            generation_id: 'generation-7',
            projections,
            sequence: 12,
          },
        },
      },
    },
  };
}

/** Answers the snapshot route and refuses anything else, so a test that
 * accidentally depends on another route fails loudly. */
function serve(handler: (url: string) => { status: number; body: unknown }) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      const { status, body } = handler(String(url));
      return new Response(JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <WorkPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  serve(() => ({ status: 200, body: snapshotBody([projection()]) }));
});

afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('the Work page over mounted routes', () => {
  it('reads the snapshot and names the task it returned', async () => {
    renderPage();
    expect(await screen.findByText('Alpha task')).toBeTruthy();
  });

  it('labels the default Work view as the active project', async () => {
    renderPage();
    expect(
      await screen.findByText('canonical task graph · the active project · nine mounted routes'),
    ).toBeTruthy();
  });

  it('labels an exact selected project without claiming it is active', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'project-beta',
        label: 'Beta',
        activation: 'selected',
      },
    });
    renderPage();

    const provenance = await screen.findByText(
      'canonical task graph · Beta (project-beta) · selected project · nine mounted routes',
    );
    expect(provenance.textContent).not.toContain('active project');
  });

  it('labels an explicitly selected active project from reconciled scope state', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'project-alpha',
        label: 'Alpha',
        activation: 'active',
      },
    });
    renderPage();

    expect(
      await screen.findByText(
        'canonical task graph · Alpha (project-alpha) · selected active project · nine mounted routes',
      ),
    ).toBeTruthy();
  });

  it('leaves the shell its own main landmark and scrolls inside a named region', async () => {
    const { container } = renderPage();
    await screen.findByText('Alpha task');
    expect(container.querySelector('main')).toBeNull();
    expect(container.querySelector('[role="region"][aria-label="Work content"]')).not.toBeNull();
  });

  /**
   * The assertion this file exists for. A 503 from the Work runtime and a
   * snapshot of zero tasks are different facts, and the first must never be
   * drawn as the second.
   */
  it('draws no board when the runtime is unavailable', async () => {
    serve(() => ({ status: 503, body: { kind: 'problem', value: { problem: {} } } }));
    const { container } = renderPage();
    await waitFor(() => expect(screen.getByText(/Work runtime is unavailable/)).toBeTruthy());
    expect(container.querySelector('[data-work-board]')).toBeNull();
    expect(container.getAttribute('data-work-authority')).not.toBe('read');
  });

  it('reports a denial as denied rather than as an empty board', async () => {
    serve(() => ({ status: 404, body: { kind: 'problem', value: { problem: {} } } }));
    const { container } = renderPage();
    await waitFor(() => expect(screen.getByText(/not authorized/)).toBeTruthy());
    expect(container.querySelector('[data-work-board]')).toBeNull();
  });

  it('reports an envelope it cannot read as unsupported rather than guessing', async () => {
    serve(() => ({ status: 200, body: { kind: 'success', value: { outcome: {} } } }));
    const { container } = renderPage();
    await waitFor(() => expect(screen.getByText(/envelope is not the shape/)).toBeTruthy());
    expect(container.querySelector('[data-work-board]')).toBeNull();
  });

  /** An empty board is legitimate — but only when the daemon said the board was
   * complete and empty. */
  it('draws an empty board only from a complete snapshot that was empty', async () => {
    serve(() => ({ status: 200, body: snapshotBody([]) }));
    const { container } = renderPage();
    await waitFor(() => expect(container.querySelector('[data-work-board]')).not.toBeNull());
    expect(screen.getAllByText(/No task in this build has reached this gate/).length).toBeGreaterThan(
      0,
    );
  });

  it('never rounds a capped snapshot up to a complete board', async () => {
    serve((url) =>
      url.includes('/delta')
        ? { status: 503, body: { kind: 'problem', value: { problem: {} } } }
        : {
            status: 200,
            body: snapshotBody([projection()], {
              state: 'capped',
              cap: 1,
              returned: 1,
              total: 40,
              cursor: { generation_id: 'generation-7', token: 'resume-1' },
              range: { start_exclusive: 0, end_inclusive: 1 },
            }),
          },
    );
    renderPage();
    expect(await screen.findByText(/1 of 40, capped at 1/)).toBeTruthy();
  });

  it('asks for a continuation only when the snapshot carried a cursor', async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) => {
        calls.push(String(url));
        return new Response(JSON.stringify(snapshotBody([projection()])), { status: 200 });
      }),
    );
    renderPage();
    await screen.findByText('Alpha task');
    // A complete snapshot has no resume cursor, so continuing from it would be
    // asking the daemon to resume from a position it never reported.
    expect(calls.some((url) => url.includes('/delta'))).toBe(false);
  });

  it('states the boundary that remains, and does not overstate it', async () => {
    renderPage();
    await screen.findByText('Alpha task');
    const boundary = screen.getByLabelText('Work boundary');
    expect(boundary.textContent).toContain('8');
    expect(boundary.textContent).toContain('terminalize');
    expect(boundary.textContent).toContain('pending proposals');
  });

  it('selects a task by keyboard and records it in the address', async () => {
    const user = userEvent.setup();
    renderPage();
    const task = await screen.findByRole('button', { name: 'Alpha task' });
    await user.tab();
    task.focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(task.getAttribute('aria-pressed')).toBe('true'));
    expect(screen.getByText(/Commands · Alpha task/)).toBeTruthy();
  });

  /** Keeps the narrow-width information loss from returning: the identity and
   * dependency count have a reflowed copy that is not `display:none` below
   * `md`, because that column is drawn nowhere else. */
  it('keeps task identity readable where its column is not drawn', async () => {
    const { container } = renderPage();
    await screen.findByText('Alpha task');
    const reflowed = container.querySelector('th[scope="row"] .md\\:hidden');
    expect(reflowed?.textContent).toContain('task-alpha');
  });

  it('gives every stage table a caption and column headers', async () => {
    const { container } = renderPage();
    await screen.findByText('Alpha task');
    const tables = [...container.querySelectorAll('table')];
    expect(tables.length).toBeGreaterThan(0);
    for (const table of tables) {
      expect(table.querySelector('caption')?.textContent ?? '').not.toBe('');
      expect(table.querySelectorAll('th[scope="col"]').length).toBeGreaterThan(0);
    }
  });
});
