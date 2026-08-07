/**
 * The four plan 11c Work projections, over the one mounted snapshot route.
 *
 * Two invariants carry this file.
 *
 * The first is plan 11's mandate: one canonical selection, many synchronized
 * projections. The switcher moves the camera and must never move the
 * selection, so a task selected on the board is still selected after three
 * projection changes and a reload of the same address.
 *
 * The second is 11c's honesty rule. Each of these projections is asked to
 * encode a measurement this build cannot take, and the failure mode is not a
 * broken drawing — it is a gap that quietly acquires a value. Every projection
 * is therefore asserted to render its absent channels as stated absences, and
 * a refusal from the daemon is asserted never to render as a projection of
 * nothing.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../../data/scope/store.ts';
import { WorkPage } from '../WorkPage.tsx';

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

function evidence(runId: string, terminal: boolean) {
  return { run_id: runId, evidence_digest: `digest-${runId}`, terminal };
}

/**
 * A graph with one of everything the projections have to read: a chain, a
 * declared cycle, an off-page dependency, a run that crossed two tasks, a
 * retry, a task no run touched, and a dependent that finished while its
 * dependency had not.
 */
const GRAPH = [
  projection({ task_id: 'root', title: 'Root task' }),
  projection({
    task_id: 'middle',
    title: 'Middle task',
    dependencies: ['root'],
    runtime_evidence: [evidence('run-1', false), evidence('run-1', true)],
  }),
  projection({
    task_id: 'leaf',
    title: 'Leaf task',
    dependencies: ['middle', 'offpage'],
    runtime_evidence: [evidence('run-1', true)],
  }),
  projection({ task_id: 'loop-a', title: 'Loop A', dependencies: ['loop-b'] }),
  projection({ task_id: 'loop-b', title: 'Loop B', dependencies: ['loop-a'] }),
  projection({ task_id: 'lonely', title: 'Lonely task' }),
];

function snapshotBody(projections: readonly unknown[]) {
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
            coverage: {
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

function renderPage(entry = '/work') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <WorkPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

async function open(name: string) {
  const user = userEvent.setup();
  await user.click(await screen.findByRole('tab', { name }));
  return user;
}

beforeEach(() => {
  serve(() => ({ status: 200, body: snapshotBody(GRAPH) }));
});

afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('the projection switcher', () => {
  it('offers every projection as a tab and opens on the board', async () => {
    renderPage();
    const tablist = await screen.findByRole('tablist', { name: 'Work projection' });
    const tabs = within(tablist).getAllByRole('tab');

    expect(tabs.map((tab) => tab.textContent)).toEqual([
      'Board',
      'DAG',
      'Timeline',
      'Causal',
      'Workload',
    ]);
    expect(within(tablist).getByRole('tab', { selected: true }).textContent).toBe('Board');
  });

  it('moves the camera between projections with the arrow keys', async () => {
    const user = userEvent.setup();
    renderPage();
    const board = await screen.findByRole('tab', { name: 'Board' });
    board.focus();

    await user.keyboard('{ArrowRight}');
    await waitFor(() =>
      expect(screen.getByRole('tab', { name: 'DAG' }).getAttribute('aria-selected')).toBe('true'),
    );
    await user.keyboard('{End}');
    await waitFor(() =>
      expect(
        screen.getByRole('tab', { name: 'Workload' }).getAttribute('aria-selected'),
      ).toBe('true'),
    );
  });

  /** The plan 11 mandate. A projection is a camera position; it does not own
   * the selection and must not clear one. */
  it('keeps the selected task across every projection change', async () => {
    const user = userEvent.setup();
    const { container } = renderPage();
    await user.click(await screen.findByRole('button', { name: 'Root task' }));
    await waitFor(() =>
      expect(container.querySelector('[data-work-task="root"][data-selected]')).not.toBeNull(),
    );

    for (const name of ['DAG', 'Timeline', 'Causal', 'Workload', 'Board']) {
      await open(name);
      await waitFor(() =>
        expect(
          container.querySelector('[data-work-task="root"][aria-pressed="true"], [data-work-task="root"][data-selected]'),
        ).not.toBeNull(),
      );
    }
  });

  it('reopens the projection its address names', async () => {
    const { container } = renderPage('/work?view=causal');
    await waitFor(() =>
      expect(container.querySelector('[data-work-view="causal"]')).not.toBeNull(),
    );
    expect(screen.getByRole('tab', { selected: true }).textContent).toBe('Causal');
  });

  /** An unreadable camera position opens the board rather than an empty frame:
   * the board is the one projection whose every channel this build measures. */
  it('opens the board when the address names a projection this build has not got', async () => {
    const { container } = renderPage('/work?view=cortex-9');
    await waitFor(() => expect(container.querySelector('[data-work-board]')).not.toBeNull());
    expect(screen.getByRole('tab', { selected: true }).textContent).toBe('Board');
  });

  /** Losing the switcher on a refusal would strand a reader in a projection
   * they cannot leave. */
  it('keeps the camera reachable when the read refuses, and draws no projection', async () => {
    serve(() => ({ status: 503, body: { kind: 'problem', value: { problem: {} } } }));
    const { container } = renderPage('/work?view=dag');

    await waitFor(() => expect(screen.getByText(/Work runtime is unavailable/)).toBeTruthy());
    expect(screen.getByRole('tablist', { name: 'Work projection' })).toBeTruthy();
    expect(container.querySelector('[data-work-view]')).toBeNull();
    expect(container.querySelector('[role="tabpanel"]')).toBeNull();
  });
});

describe('the DAG projection', () => {
  it('layers the declared graph and names the deepest chain', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() => expect(container.querySelector('[data-work-view="dag"]')).not.toBeNull());

    // root -> middle -> leaf is three strata; the cycle and the lonely task
    // both sit at depth 0 with root.
    expect(container.querySelector('[data-work-task="root"]')?.getAttribute('data-work-depth')).toBe(
      '0',
    );
    expect(
      container.querySelector('[data-work-task="leaf"]')?.getAttribute('data-work-depth'),
    ).toBe('2');
    expect(container.querySelectorAll('[data-work-widest="true"]').length).toBeGreaterThan(0);
  });

  it('condenses a declared cycle and states that it is an observation', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() => expect(container.querySelector('[data-work-cycle]')).not.toBeNull());

    expect(container.querySelector('[data-work-cycle]')?.getAttribute('data-work-cycle')).toBe('2');
    expect(screen.getByText(/not an error in this drawing/)).toBeTruthy();
  });

  it('lists a dependency the snapshot did not return rather than dropping it', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() => expect(container.querySelector('[data-work-view="dag"]')).not.toBeNull());

    expect(screen.getByText('leaf needs offpage')).toBeTruthy();
  });

  /** The effort-weighted critical path is the measurement 11c asks for and no
   * contract in this build carries it. */
  it('states that it could not weight the critical path', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() => expect(container.querySelector('[data-work-view="dag"]')).not.toBeNull());

    expect(
      container.querySelector('[data-work-measure="effort-weighted critical path"]'),
    ).not.toBeNull();
  });
});

describe('every attempt-shaped projection', () => {
  /**
   * The assertion this file exists for. Effort, wall clock, observed order,
   * executor identity, concurrency and churn are all absent in this build, and
   * a projection that drew one of them would be drawing a number nobody could
   * check.
   */
  it.each([
    ['DAG', 'dag'],
    ['Timeline', 'timeline'],
    ['Causal', 'causal'],
    ['Workload', 'workload'],
  ])('%s states the measurements it could not take', async (name, view) => {
    const { container } = renderPage(`/work?view=${view}`);
    await waitFor(() =>
      expect(container.querySelector(`[data-work-view="${view}"]`)).not.toBeNull(),
    );

    const absences = container.querySelectorAll('[data-work-channel="absent"]');
    expect(absences.length).toBeGreaterThan(0);
    for (const absence of absences) {
      expect((absence.textContent ?? '').length).toBeGreaterThan(40);
    }
    expect(name.length).toBeGreaterThan(0);
  });

  it.each([
    ['Timeline', 'timeline'],
    ['Causal', 'causal'],
    ['Workload', 'workload'],
  ])('%s draws an empty board as an empty board, not as a failure', async (name, view) => {
    serve(() => ({ status: 200, body: snapshotBody([]) }));
    const { container } = renderPage(`/work?view=${view}`);

    await waitFor(() =>
      expect(container.querySelector(`[data-work-view="${view}"]`)).not.toBeNull(),
    );
    expect(container.querySelector('[data-work-reading="empty"]')).not.toBeNull();
    expect(name.length).toBeGreaterThan(0);
  });

  /** Any table these projections draw is read by a screen reader, so it needs
   * a caption and column headers like every other table in the workspace. */
  it.each([
    ['DAG', 'dag'],
    ['Timeline', 'timeline'],
    ['Causal', 'causal'],
    ['Workload', 'workload'],
  ])('%s captions every table it draws', async (name, view) => {
    const { container } = renderPage(`/work?view=${view}`);
    await waitFor(() =>
      expect(container.querySelector(`[data-work-view="${view}"]`)).not.toBeNull(),
    );

    for (const table of container.querySelectorAll('table')) {
      expect(table.querySelector('caption')?.textContent ?? '').not.toBe('');
      expect(table.querySelectorAll('th[scope="col"]').length).toBeGreaterThan(0);
    }
    expect(name.length).toBeGreaterThan(0);
  });

  /** 44px explicitly: the app's root font size is 14px, so a spacing-11
   * minimum computes to 38.5px and lands under the target size the
   * accessibility gate measures. */
  it.each([
    ['DAG', 'dag'],
    ['Timeline', 'timeline'],
    ['Causal', 'causal'],
    ['Workload', 'workload'],
  ])('%s gives every task control a reachable target', async (name, view) => {
    const { container } = renderPage(`/work?view=${view}`);
    await waitFor(() =>
      expect(container.querySelector(`[data-work-view="${view}"]`)).not.toBeNull(),
    );

    const controls = container.querySelectorAll(`[data-work-view="${view}"] [data-work-task]`);
    // Anti-vacuity: a projection that drew no task control at all would pass
    // the loop below without measuring anything, and every projection in this
    // fixture has tasks to draw.
    expect(controls.length).toBeGreaterThan(0);
    for (const control of controls) {
      expect(control.className).toContain('min-h-[44px]');
    }
    expect(name.length).toBeGreaterThan(0);
  });
});
