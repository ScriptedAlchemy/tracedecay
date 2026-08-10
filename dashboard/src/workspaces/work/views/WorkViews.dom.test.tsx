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
import {
  workAttempt,
  workAttemptList,
  workRoute,
  workTerminal,
} from '../../../test/workAttemptFixture.ts';
import { workGraphRead, type WorkGraphVersionSpec } from '../../../test/workGraphFixture.ts';
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
    task_accepted: false,
    task_id: 'task-alpha',
    title: 'Alpha task',
    version: 4,
    ...overrides,
  };
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
  }),
  projection({
    task_id: 'leaf',
    title: 'Leaf task',
    dependencies: ['middle', 'offpage'],
  }),
  projection({ task_id: 'loop-a', title: 'Loop A', dependencies: ['loop-b'] }),
  projection({ task_id: 'loop-b', title: 'Loop B', dependencies: ['loop-a'] }),
  projection({ task_id: 'lonely', title: 'Lonely task' }),
];

/** The application envelope every Work route answers in. */
function workEnvelope(payload: unknown, bindingId: string) {
  return {
    kind: 'success',
    value: {
      binding_id: bindingId,
      contract: { schema_id: 'schema.work.result', schema_revision: 1 },
      request_id: 'request-1',
      scope: {},
      outcome: { outcome: 'evidence', value: { payload } },
    },
  };
}

function snapshotBody(projections: readonly unknown[], generation = 'generation-7') {
  return workEnvelope(
    {
      coverage: { state: 'complete', returned: projections.length, total: projections.length },
      generation_id: generation,
      projections,
      sequence: 12,
    },
    'binding.http.work.snapshot',
  );
}

/**
 * The execution record behind the graph above: one task that took two attempts
 * and only succeeded after the fallback route took over, and one attempt part
 * way up the cancellation ladder that has not terminated.
 */
const ATTEMPTS = [
  workAttempt({
    taskId: 'middle',
    runId: 'run-1',
    attemptId: 'attempt-1',
    state: 'failed',
    terminal: workTerminal('failed', 100),
  }),
  workAttempt({
    taskId: 'middle',
    runId: 'run-1',
    attemptId: 'attempt-2',
    actual: workRoute('claude', 'route-fallback'),
    recovery: { reason: 'lease_lost', source_attempt_id: 'attempt-1', state: 'restarted' },
    terminal: workTerminal('succeeded', 200),
  }),
  workAttempt({
    taskId: 'leaf',
    runId: 'run-1',
    attemptId: 'attempt-3',
    state: 'cancellation_escalated',
    cancellation: {
      state: 'escalated',
      value: {
        acknowledgement: { acknowledged_at: 12, request: { request_id: 'c-1', requested_at: 8 } },
        escalated_at: 20,
      },
    },
    terminal: null,
  }),
];

/**
 * The work-product graph behind the projections, coherent with GRAPH above: the
 * same chain carries declared effort, its dependencies are the gating edges,
 * one causal candidate is nominated, and one attempt is live under complete
 * runtime coverage.
 */
const VIEWS_GRAPH: WorkGraphVersionSpec = {
  tasks: [
    { taskId: 'root', effort: 2 },
    { taskId: 'middle', effort: 3, dependencies: ['root'] },
    { taskId: 'leaf', effort: 5, dependencies: ['middle'], causalCandidates: ['middle'] },
    { taskId: 'loop-a', dependencies: ['loop-b'] },
    { taskId: 'loop-b', dependencies: ['loop-a'] },
    { taskId: 'lonely' },
  ],
  criticalPath: ['root', 'middle', 'leaf'],
  runtimeAttempts: [{ attemptId: 'attempt-2', taskId: 'middle', runId: 'run-1' }],
};

function viewsBody(spec: WorkGraphVersionSpec = VIEWS_GRAPH) {
  return {
    status: 200,
    body: workEnvelope(workGraphRead(spec), 'binding.http.work.views'),
  };
}

/** The generated `ExecutionTopologyViewV1` payload behind the topology lens.
 * Its policy dimensions are deliberately unlike anything the attempt envelope
 * could reconstruct, which makes the DOM test prove the mounted canonical
 * route is the one the view consumed. */
function topologyBody(
  generation = 'generation-7',
  lanes: readonly unknown[] = [
    {
      task_id: 'middle',
      run_id: 'run-1',
      attempt_count: 2,
      placement: {
        state: 'placed',
        placement: {
          authority_version: 4,
          blockers: [],
          identity: { task_id: 'middle', run_id: 'run-1' },
          retention_eligible_at: null,
          state: 'admitted',
          target: {
            kind: 'linked_worktree',
            in_place_acknowledged: false,
            network_free: true,
            root: '/w/main',
          },
          transitioned_at: 10,
        },
      },
    },
    {
      task_id: 'leaf',
      run_id: 'run-1',
      attempt_count: 1,
      placement: { state: 'absent' },
    },
  ],
) {
  return {
    status: 200,
    body: workEnvelope(
      {
        state: 'view',
        topology: { generation, task_count: 6 },
        coverage: { coverage: 'complete', returned: 3 },
        execution_placement: { mode: { kind: 'existing_worktree_only' }, lanes },
        branch_topology: { allowed: ['unbranched', 'independent_branches'] },
        review_topology: {
          allowed: ['no_review', 'standard_pull_requests'],
          github_stacked_prs: 'disabled',
        },
        integration_strategy: {
          cross_merge: {
            allow_cross_repository: false,
            allowed_modes: ['disabled', 'fast_forward_only'],
            default_mode: 'fast_forward_only',
          },
          gates: {
            cleanliness: 'require_clean',
            maximum_preflight_age_seconds: 300,
            require_fresh_preflight: true,
            review: { kind: 'independent_review_count', count: 1 },
            tests: [],
          },
          protected_refs: [],
        },
      },
      'binding.http.work.topology',
    ),
  };
}

/** Serve every Work read the page issues. Routed by path rather than
 * answered with one body, so a projection cannot pass by reading the wrong
 * contract. */
function serveWork(
  attempts: { status: number; body: unknown } = {
    status: 200,
    body: workEnvelope(workAttemptList(ATTEMPTS), 'binding.http.work.list_attempts'),
  },
  projections: readonly unknown[] = GRAPH,
  views: { status: number; body: unknown } = viewsBody(),
  topology: { status: number; body: unknown } = topologyBody(),
  snapshotGeneration = 'generation-7',
) {
  serve((url) =>
    url.includes('/work/list-attempts')
      ? attempts
      : url.includes('/work/views')
        ? views
        : url.includes('/work/topology')
          ? topology
        : { status: 200, body: snapshotBody(projections, snapshotGeneration) },
  );
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

const ID_REFERENCES = ['aria-controls', 'aria-labelledby', 'aria-describedby'] as const;

/**
 * Every id an ARIA reference on the page names but the page did not draw.
 *
 * The accessibility gate reads references, not intentions: an `aria-controls`
 * naming an absent element is a critical `aria-valid-attr-value` failure, not
 * a control that merely happens to point at nothing. This returns the offences
 * rather than a boolean so a failure names the attribute that broke.
 */
function danglingReferences(container: HTMLElement): string[] {
  const offences: string[] = [];
  const selector = ID_REFERENCES.map((attribute) => `[${attribute}]`).join(',');
  for (const element of Array.from(container.querySelectorAll(selector))) {
    for (const attribute of ID_REFERENCES) {
      const value = element.getAttribute(attribute);
      if (value === null) continue;
      for (const id of value.split(/\s+/).filter((token) => token !== '')) {
        // Resolved against the document, the way an assistive technology
        // resolves an IDREF — not against this subtree.
        if (element.ownerDocument.getElementById(id) === null) {
          offences.push(`${element.tagName.toLowerCase()} ${attribute}="${id}"`);
        }
      }
    }
  }
  return offences;
}

beforeEach(() => {
  serveWork();
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
      'Topology',
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
        screen.getByRole('tab', { name: 'Topology' }).getAttribute('aria-selected'),
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

  it('resolves every ARIA reference it makes while a snapshot is drawn', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() => expect(container.querySelector('[data-work-view="dag"]')).not.toBeNull());

    expect(danglingReferences(container)).toEqual([]);
  });

  /**
   * Losing the switcher on a refusal would strand a reader in a projection
   * they cannot leave — so the tabs stay. Which means the region they name has
   * to stay with them: tabs that keep `aria-controls` pointed at a panel the
   * refusal branch never drew are a dangling reference, and the accessibility
   * gate reads that as a critical invalid attribute value rather than as a
   * projection that is merely absent.
   */
  it('keeps the camera and the region it controls when the read refuses', async () => {
    serve(() => ({ status: 503, body: { kind: 'problem', value: { problem: {} } } }));
    const { container } = renderPage('/work?view=dag');

    await waitFor(() => expect(screen.getByText(/Work runtime is unavailable/)).toBeTruthy());
    expect(screen.getByRole('tablist', { name: 'Work projection' })).toBeTruthy();
    expect(container.querySelector('[data-work-view]')).toBeNull();

    // The refusal is what the camera is now pointed at, so it belongs inside
    // the region the tabs control rather than beside it.
    const panel = screen.getByRole('tabpanel');
    expect(within(panel).getByText(/Work runtime is unavailable/)).toBeTruthy();
    expect(danglingReferences(container)).toEqual([]);
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

  /** The effort-weighted critical path is the measurement 11c asks for, and
   * `operation.work.views` now carries it: the authority's chain and its
   * weight render from the wire, beside — never in place of — the unweighted
   * chain over this page's own edges. */
  it('weights the critical path with the authority chain from the graph read', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() =>
      expect(container.querySelector('[data-work-critical-path="3"]')).not.toBeNull(),
    );

    expect(
      container
        .querySelector('[data-work-critical-path="3"]')
        ?.getAttribute('data-work-critical-effort'),
    ).toBe('10');
    // The unweighted chain over this page's edges survives beside it.
    expect(container.querySelectorAll('[data-work-widest="true"]').length).toBeGreaterThan(0);
  });

  it('lists the gating edges the graph declares', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() => expect(container.querySelector('[data-work-gating]')).not.toBeNull());

    expect(container.querySelector('[data-work-gating]')?.getAttribute('data-work-gating')).toBe(
      '4',
    );
    expect(screen.getByText('middle needs root')).toBeTruthy();
  });

  /** When the graph read is refused, the two graph-fed panels state the
   * refusal in the read's own words rather than drawing an empty chain. */
  it('states a refused graph read on the channels it would have fed', async () => {
    serveWork(undefined, GRAPH, {
      status: 503,
      body: { kind: 'problem' },
    });
    const { container } = renderPage('/work?view=dag');
    await waitFor(() =>
      expect(container.querySelector('[data-work-critical-path="absent"]')).not.toBeNull(),
    );

    expect(
      container.querySelector('[data-work-critical-path="absent"]')?.textContent ?? '',
    ).toContain('refused');
  });
});

describe('every attempt-shaped projection', () => {
  /**
   * The assertion this file exists for. Wall clock and observed execution
   * order survived the graph-read mount as the two measurements no contract
   * carries — an attempt still records an end and no start, and nothing binds
   * a task's completion to the instant another task finished — and a
   * projection that drew either would be drawing a number nobody could check.
   */
  it.each([
    ['Timeline', 'timeline'],
    ['Causal', 'causal'],
    ['Topology', 'topology'],
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
    serveWork(undefined, []);
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
    ['Topology', 'topology'],
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

/**
 * The graph-fed channels, over the mounted views route.
 *
 * Effort, concurrency, churn, gating edges, causal candidates, task instants
 * and the live runtime projection were this page's absences until
 * `operation.work.views` landed. Each test asserts a measurement is now drawn
 * from `WorkGraphReadV1` on the wire — parsed by the same schema the model
 * tests prove the fixture against — and that the read's own limits are still
 * said out loud: declared-and-empty is an answer, withheld is not zero, and
 * unmeasured attempts are never a quiet empty list.
 */
describe('the graph-fed channels', () => {
  it('draws the workload figures the graph version answered', async () => {
    const { container } = renderPage('/work?view=workload');
    await waitFor(() => expect(container.querySelector('[data-work-effort]')).not.toBeNull());

    // Total declared effort: 2+3+5 on the chain, 1 each for the three others.
    expect(container.querySelector('[data-work-effort]')?.getAttribute('data-work-effort')).toBe(
      '13',
    );
    expect(
      container.querySelector('[data-work-effort-split]')?.getAttribute('data-work-effort-split'),
    ).toBe('13/0/0');
    expect(
      container.querySelector('[data-work-concurrency]')?.getAttribute('data-work-concurrency'),
    ).toBe('1/2');
    // Every fixture task was last updated two days before the read instant, so
    // a one-day churn window truthfully lists nothing over six counted tasks.
    expect(container.querySelector('[data-work-churn]')?.getAttribute('data-work-churn')).toBe('0');
    expect(
      container.querySelector('[data-work-churn-counted]')?.getAttribute('data-work-churn-counted'),
    ).toBe('6');
    expect(
      container.querySelector('[data-work-runtime="complete"]')?.getAttribute(
        'data-work-runtime-attempts',
      ),
    ).toBe('1');
  });

  /** Unavailable runtime coverage is unmeasured attempts. It must never render
   * like the complete-and-empty reading, which is the authority stating that
   * nothing is running. */
  it('renders unmeasured attempts as unmeasured, never as zero attempts', async () => {
    serveWork(undefined, GRAPH, viewsBody({
      ...VIEWS_GRAPH,
      runtimeAttempts: [],
      runtimeCoverage: { coverage: 'unavailable' },
    }));
    const { container } = renderPage('/work?view=workload');
    await waitFor(() =>
      expect(container.querySelector('[data-work-runtime="unavailable"]')).not.toBeNull(),
    );

    expect(
      container.querySelector('[data-work-runtime="unavailable"]')?.textContent ?? '',
    ).toContain('not a reading of zero attempts');
    expect(container.querySelector('[data-work-runtime-attempts]')).toBeNull();
    expect(container.querySelector('[data-work-aggregation]')).toBeNull();
    expect(container.querySelector('[data-work-unattributed]')).toBeNull();
    expect(container.textContent).not.toContain('No run has attached evidence');
  });

  it('does not render unavailable board attempt stages as zero attempts', async () => {
    serveWork(undefined, GRAPH, viewsBody({
      ...VIEWS_GRAPH,
      runtimeAttempts: [],
      runtimeCoverage: { coverage: 'unavailable' },
    }));
    const { container } = renderPage('/work?view=board');
    await waitFor(() =>
      expect(container.querySelector('[data-work-board-attempts="unavailable"]')).not.toBeNull(),
    );

    expect(
      container.querySelector('[data-work-board-attempts="unavailable"]')?.textContent ?? '',
    ).toContain('unmeasured, not zero');
  });

  it('draws the declared causal candidates from the wire', async () => {
    const { container } = renderPage('/work?view=causal');
    await waitFor(() => expect(container.querySelector('[data-work-candidates]')).not.toBeNull());

    expect(
      container.querySelector('[data-work-candidates]')?.getAttribute('data-work-candidates'),
    ).toBe('1');
  });

  /** The one channel whose empty value is a reading, and the one fill-in the
   * mount must never cause: a graph that declares no candidate renders the
   * declaration, and the observed order stays a stated absence beside it. */
  it('renders declared-none candidates as an answer while order stays absent', async () => {
    serveWork(undefined, GRAPH, viewsBody({
      ...VIEWS_GRAPH,
      tasks: VIEWS_GRAPH.tasks.map((task) => ({ ...task, causalCandidates: [] })),
    }));
    const { container } = renderPage('/work?view=causal');
    await waitFor(() =>
      expect(screen.getByText(/declares no causal candidate at all/)).toBeTruthy(),
    );

    const absences = [...container.querySelectorAll('[data-work-channel="absent"]')].map(
      (absence) => absence.getAttribute('data-work-measure') ?? '',
    );
    expect(absences.some((measure) => measure.includes('order'))).toBe(true);
  });

  it('places the recorded task instants on the timeline', async () => {
    const { container } = renderPage('/work?view=timeline');
    await waitFor(() => expect(container.querySelector('[data-work-instants]')).not.toBeNull());

    expect(
      container.querySelector('[data-work-instants]')?.getAttribute('data-work-instants'),
    ).toBe('6');
    // Six calendar rows and still no span anywhere.
    expect(container.querySelector('[data-work-span="hollow"]')).not.toBeNull();
  });
});

/**
 * The execution record, over the mounted attempt-list route.
 *
 * These four readings were the timeline's absences until the route landed, so
 * the tests here are the mirror image of the ones above: each asserts that a
 * measurement is now drawn from `WorkAttemptV1` rather than inferred, and that
 * the page's own limits — a cap, a refusal, a typed absence — are still said
 * out loud instead of collapsing into an empty record.
 */
describe('the execution record', () => {
  async function openTimeline() {
    const page = renderPage('/work?view=timeline');
    await waitFor(() =>
      expect(page.container.querySelector('[data-work-execution-record]')).not.toBeNull(),
    );
    return page.container;
  }

  it('names the route that actually ran each attempt, and the diversion to it', async () => {
    const container = await openTimeline();

    const fallback = container.querySelector('[data-work-executor="claude/route-fallback"]');
    expect(fallback?.getAttribute('data-work-executor-attempts')).toBe('1');
    // The attempt asked for codex and ran on claude: attributed where it ran,
    // and counted as a diversion so the row cannot be read as a plain choice.
    expect(fallback?.getAttribute('data-work-executor-diverted')).toBe('1');
    expect(
      container.querySelector('[data-work-executor="codex/route-primary"]')
        ?.getAttribute('data-work-executor-attempts'),
    ).toBe('2');
  });

  /** The weave counts evidence rows and calls a repeat a retry. This counts
   * links in a recovery chain, which is the measured version of the same
   * claim — `middle` took two attempts and the second descends from the first. */
  it('draws the retry chain from attempt descent rather than evidence incidence', async () => {
    const container = await openTimeline();

    const lineage = container.querySelector('[data-work-lineage="middle/run-1"]');
    expect(lineage?.getAttribute('data-work-restarts')).toBe('1');
    expect(lineage?.getAttribute('data-work-lineage-truncated')).toBeNull();
    expect(container.querySelectorAll('[data-work-link]').length).toBe(3);
    expect(
      container.querySelector('[data-work-link="attempt-2"]')?.getAttribute('data-work-link-origin'),
    ).toBe('restarted');
  });

  it('counts the furthest cancellation rung each attempt reached', async () => {
    const container = await openTimeline();

    expect(
      container.querySelector('[data-work-ladder-rung="escalated"]')
        ?.getAttribute('data-work-ladder-count'),
    ).toBe('1');
    // An empty rung is still drawn: a ladder whose shape depended on its own
    // values could not be told from a ladder with fewer rungs.
    expect(
      container.querySelector('[data-work-ladder-rung="requested"]')
        ?.getAttribute('data-work-ladder-count'),
    ).toBe('0');
  });

  /**
   * The one measurement of time this build can make, and the one it still
   * cannot. Two attempts terminated so two hold a place in the order; the
   * attempt still climbing the cancellation ladder holds none. No duration is
   * drawn from those instants, and the weave says so.
   */
  it('orders terminated attempts by observation and still refuses a duration', async () => {
    const container = await openTimeline();

    expect(
      container.querySelector('[data-work-terminal-order]')?.getAttribute('data-work-terminal-order'),
    ).toBe('2');
    expect(container.querySelector('[data-work-measure="wall-clock spans and durations"]'))
      .not.toBeNull();
  });

  it('states a capped page as a floor rather than totalling what it did not read', async () => {
    serveWork({
      status: 200,
      body: workEnvelope(
        workAttemptList(ATTEMPTS, {
          coverage: 'capped',
          remaining: 41,
          resume: {
            generation: 'generation-7',
            start_after: { attempt_id: 'attempt-3', run_id: 'run-1', task_id: 'leaf' },
          },
          returned: 3,
        }),
        'binding.http.work.list_attempts',
      ),
    });
    const container = await openTimeline();

    expect(container.querySelector('[data-work-attempt-coverage="capped"]')).not.toBeNull();
    expect(screen.getByText(/3 of 44 attempts/)).toBeTruthy();
    expect(screen.getByText(/every count below is a floor/)).toBeTruthy();
  });

  /**
   * A cursor minted under a superseded topology generation is refused, and the
   * refusal has to reach the page as a refusal. An execution record that fell
   * back to an empty page would report "nothing ran", which is the opposite of
   * what happened.
   */
  it('draws a refused attempt read as a refusal, never as an empty record', async () => {
    serveWork({ status: 409, body: { kind: 'problem', value: { problem: {} } } });
    const container = await openTimeline();

    const record = container.querySelector<HTMLElement>('[data-work-execution-record]');
    expect(record?.getAttribute('data-work-execution-record')).toBe('refused');
    // The daemon's sentence, inside the record rather than only on its chip.
    expect(within(record as HTMLElement).getByText(/the task moved since it was read/)).toBeTruthy();
    // Nothing measured is drawn: no executor row, no chain, no ladder.
    expect(container.querySelector('[data-work-executor]')).toBeNull();
    expect(container.querySelector('[data-work-lineage]')).toBeNull();
    expect(container.querySelector('[data-work-ladder-rung]')).toBeNull();
  });

  /** The daemon's typed `absent`, which its policy makes indistinguishable
   * from a denial. Reported as the one state it arrived as. */
  it('reports a typed absence as an absence its policy will not disambiguate', async () => {
    serveWork({
      status: 200,
      body: workEnvelope({ state: 'absent' }, 'binding.http.work.list_attempts'),
    });
    const container = await openTimeline();

    expect(
      container.querySelector('[data-work-execution-record]')?.getAttribute(
        'data-work-execution-record',
      ),
    ).toBe('absent');
    expect(screen.getByText(/indistinguishable from a denial/)).toBeTruthy();
  });

  it('resolves every ARIA reference it makes while the record is drawn', async () => {
    const container = await openTimeline();
    expect(danglingReferences(container)).toEqual([]);
  });
});

/** The canonical `operation.work.topology` route publishes every structural
 * dimension under one generation. The DOM assertions below prove the mounted
 * route is consumed, rather than a browser reconstruction from attempt data. */
describe('the execution-topology lens', () => {
  async function openTopology() {
    const page = renderPage('/work?view=topology');
    await waitFor(() =>
      expect(page.container.querySelector('[data-work-view="topology"]')).not.toBeNull(),
    );
    return page.container;
  }

  it('pins the verified topology generation the page was read under', async () => {
    const container = await openTopology();
    await waitFor(() =>
      expect(
        container.querySelector('[data-work-topology-generation="generation-7"]'),
      ).not.toBeNull(),
    );
  });

  it('renders placement lanes and every policy dimension from the canonical topology payload', async () => {
    const container = await openTopology();
    await waitFor(() => expect(container.querySelector('[data-work-topology-lanes="2"]')).not.toBeNull());

    const lane = container.querySelector<HTMLElement>('[data-work-topology-lane="middle:run-1"]');
    expect(lane).not.toBeNull();
    expect(within(lane as HTMLElement).getByText(/linked_worktree/)).toBeTruthy();
    expect(within(lane as HTMLElement).getByText(/\/w\/main/)).toBeTruthy();
    expect(
      container.querySelector('[data-work-topology-lane="leaf:run-1"] [data-state="complete_zero_findings"]'),
    ).not.toBeNull();

    for (const [dimension, detail] of [
      ['branch_topology', 'independent_branches'],
      ['review_topology', 'standard_pull_requests'],
      ['integration_strategy', 'fast_forward_only'],
    ] as const) {
      const row = container.querySelector<HTMLElement>(`[data-work-dimension="${dimension}"]`);
      expect(row).not.toBeNull();
      expect(within(row as HTMLElement).getByText('Ready')).toBeTruthy();
      expect(row?.textContent).toContain(detail);
    }
  });

  it('draws a refused canonical topology read as a refusal, never as an empty lane set', async () => {
    serveWork(undefined, GRAPH, viewsBody(), { status: 503, body: { kind: 'problem' } });
    const container = await openTopology();

    expect(container.querySelector('[data-work-topology-lane]')).toBeNull();
    const absences = container.querySelectorAll('[data-work-channel="absent"]');
    expect(absences.length).toBeGreaterThan(0);
    expect(
      Array.from(absences).some((absence) =>
        (absence.textContent ?? '').includes('the Work runtime is unavailable'),
      ),
    ).toBe(true);
  });

  it('draws an authorized empty canonical page as a statement, not a failed render', async () => {
    serveWork(undefined, GRAPH, viewsBody(), topologyBody('generation-7', []));
    const container = await openTopology();

    await waitFor(() =>
      expect(container.querySelector('[data-work-reading="empty"]')).not.toBeNull(),
    );
    // Empty is still pinned: the generation identity arrived with the page.
    expect(
      container.querySelector('[data-work-topology-generation="generation-7"]'),
    ).not.toBeNull();
  });

  it('selects a canonical placement lane and keeps that selection when the camera moves', async () => {
    const container = await openTopology();
    const user = userEvent.setup();
    await waitFor(() => expect(container.querySelector('[data-work-topology-lane]')).not.toBeNull());

    const landing = container.querySelector<HTMLElement>(
      '[data-work-topology-lane="middle:run-1"] [data-work-task="middle"]',
    );
    expect(landing).not.toBeNull();
    await user.click(landing as HTMLElement);
    await waitFor(() =>
      expect(landing?.getAttribute('aria-pressed')).toBe('true'),
    );

    // The canonical selection survives the camera move to the board.
    await user.click(screen.getByRole('tab', { name: 'Board' }));
    await waitFor(() =>
      expect(
        container.querySelector(
          '[data-work-task="middle"][aria-pressed="true"], [data-work-task="middle"][data-selected]',
        ),
      ).not.toBeNull(),
    );
  });

  it('resolves every ARIA reference it makes while the lens is drawn', async () => {
    const container = await openTopology();
    await waitFor(() => expect(container.querySelector('[data-work-topology-lane]')).not.toBeNull());
    expect(danglingReferences(container)).toEqual([]);
  });

  it('refuses graph runtime figures when the mounted canonical topology generation differs', async () => {
    serveWork(undefined, GRAPH, viewsBody(), topologyBody('generation-other'));
    const container = await openTopology();
    await waitFor(() =>
      expect(container.querySelector('[data-work-accounting="concurrency_and_fanout"]')).not.toBeNull(),
    );
    const concurrency = container.querySelector<HTMLElement>(
      '[data-work-accounting="concurrency_and_fanout"]',
    );
    expect(concurrency).not.toBeNull();
    expect(concurrency?.getAttribute('data-work-accounting-reading')).toBe('absent');
    expect(concurrency?.textContent).toContain('generation-7');
    expect(concurrency?.textContent).toContain('generation-other');
    expect(concurrency?.textContent).toContain('unbound');
  });

  it('refuses snapshot titles and attempt accounting across independently refreshed generations', async () => {
    const mismatchedAttempts = workAttemptList(ATTEMPTS);
    serveWork(
      {
        status: 200,
        body: workEnvelope(
          {
            ...mismatchedAttempts,
            topology: { ...mismatchedAttempts.topology, generation: 'generation-attempt' },
          },
          'binding.http.work.list_attempts',
        ),
      },
      GRAPH,
      viewsBody(),
      topologyBody('generation-topology'),
      'generation-snapshot',
    );
    const container = await openTopology();

    await waitFor(() =>
      expect(container.querySelector('[data-work-snapshot-title-join="conflicting"]')).not.toBeNull(),
    );
    const titleJoin = container.querySelector<HTMLElement>(
      '[data-work-snapshot-title-join="conflicting"]',
    );
    expect(titleJoin?.textContent).toContain('generation-snapshot');
    expect(titleJoin?.textContent).toContain('generation-topology');
    expect(titleJoin?.textContent).toContain('unbound');

    const lane = container.querySelector<HTMLElement>('[data-work-topology-lane="middle:run-1"]');
    expect(lane?.textContent).toContain('middle');
    expect(lane?.textContent).not.toContain('Middle task');

    await waitFor(() =>
      expect(container.querySelector('[data-work-accounting="reruns"]')).not.toBeNull(),
    );
    const reruns = container.querySelector<HTMLElement>('[data-work-accounting="reruns"]');
    expect(reruns?.getAttribute('data-work-accounting-reading')).toBe('absent');
    expect(reruns?.textContent).toContain('generation-attempt');
    expect(reruns?.textContent).toContain('generation-topology');
    expect(reruns?.textContent).toContain('unbound');
    expect(reruns?.querySelector('[data-work-accounting-value]')).toBeNull();
  });
});
