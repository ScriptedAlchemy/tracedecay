/**
 * The task dependency board over one exact graph version.
 *
 * Three invariants carry this file. The board draws only what the authority
 * served: a lane is the kanban card's word and a task without a card says so.
 * Hover and focus inspect without selecting: the address does not move until
 * click or Enter. And every drawn thing has a non-visual twin: the exact table
 * restates every card and relation, and the controls that would promise a
 * reading the authority withheld are disabled with that channel's own reason.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../../data/scope/store.ts';
import { workGraphRead, type WorkGraphVersionSpec } from '../../../test/workGraphFixture.ts';
import { WorkPage } from '../WorkPage.tsx';

function workEnvelope(payload: unknown, bindingId: string) {
  return {
    kind: 'success',
    value: {
      binding_id: bindingId,
      contract: { schema_id: 'schema.work.result', schema_revision: 1 },
      request_id: 'request-1',
      scope: {
        project_id: 'project.work',
        repository_id: 'repository.work',
        worktree_id: 'worktree.work',
        reference: null,
        scope_digest: 'sha256:scope',
      },
      outcome: { outcome: 'evidence', value: { payload } },
    },
  };
}

/**
 * A graph with two strata under one root, a soft relation of each kind, a
 * task the authority left without a kanban card, and a critical path the
 * authority weighted over root → middle → leaf.
 */
const GRAPH: WorkGraphVersionSpec = {
  tasks: [
    { taskId: 'root', title: 'Root task', effort: 2, lane: 'done' },
    { taskId: 'middle', title: 'Middle task', effort: 3, dependencies: ['root'], lane: 'ready' },
    { taskId: 'side', title: 'Side task', effort: 1, dependencies: ['root'], lane: 'blocked' },
    {
      taskId: 'leaf',
      title: 'Leaf task',
      effort: 5,
      dependencies: ['middle', 'side'],
      causalCandidates: ['side'],
      lane: 'todo',
    },
  ],
  criticalPath: ['root', 'middle', 'leaf'],
  runtimeAttempts: [{ attemptId: 'attempt-1', taskId: 'middle', runId: 'run-1', state: 'running' }],
};

type Payload = ReturnType<typeof workGraphRead>;

/** The fixture builder cards every task; the board must also survive a task
 * the authority did not card, and a declared informational relation. */
function shape(payload: Payload): Payload {
  payload.snapshot.projections.kanban.cards = payload.snapshot.projections.kanban.cards.filter(
    (card) => card.task_id !== 'side',
  );
  const leaf = payload.snapshot.graph.items.find((item) => item.input.task_id === 'leaf');
  // The fixture types the empty relation list as `never[]`; the wire accepts
  // task ids, and the schema parse on the page proves it.
  if (leaf !== undefined) {
    (leaf.input as { informational_relations: string[] }).informational_relations = ['root'];
  }
  return payload;
}

function serve(payload: unknown = shape(workGraphRead(GRAPH))) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      const target = String(url);
      const { status, body } = target.includes('/work/views')
        ? { status: 200, body: workEnvelope(payload, 'binding.http.work.views') }
        : { status: 503, body: { kind: 'problem', value: { problem: {} } } };
      return new Response(JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

const address = { search: '' };
function AddressProbe() {
  address.search = useLocation().search;
  return null;
}

function renderBoard(entry = '/work?view=dag') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <AddressProbe />
        <WorkPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

async function drawn() {
  const page = renderBoard();
  await waitFor(() =>
    expect(page.container.querySelector('[data-work-dag-board="drawn"]')).not.toBeNull(),
  );
  return page.container;
}

function card(container: HTMLElement, taskId: string): HTMLButtonElement {
  const element = container.querySelector<HTMLButtonElement>(
    `[data-work-dag-board] [data-work-task="${taskId}"][data-work-dag-card]`,
  );
  if (element === null) throw new Error(`no card for ${taskId}`);
  return element;
}

beforeEach(() => {
  serve();
});

afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('the task dependency board', () => {
  it('draws one card per task with the lane the kanban projection served', async () => {
    const container = await drawn();
    expect(container.querySelectorAll('[data-work-dag-card]')).toHaveLength(4);
    expect(card(container, 'root').textContent).toContain('DONE');
    expect(card(container, 'middle').textContent).toContain('READY');
    expect(card(container, 'leaf').textContent).toContain('TODO');
    // The authority carded no lane for `side`, and the card says so rather
    // than defaulting it into a lane.
    expect(card(container, 'side').textContent).toContain('NO CARD');
    expect(card(container, 'side').getAttribute('aria-label')).toContain('no card');
  });

  it('layers cards by longest dependency path and draws every relation kind', async () => {
    const container = await drawn();
    expect(card(container, 'root').getAttribute('data-work-depth')).toBe('0');
    expect(card(container, 'middle').getAttribute('data-work-depth')).toBe('1');
    expect(card(container, 'side').getAttribute('data-work-depth')).toBe('1');
    expect(card(container, 'leaf').getAttribute('data-work-depth')).toBe('2');

    expect(container.querySelectorAll('[data-work-dag-relation="gating"]')).toHaveLength(4);
    expect(container.querySelector('[data-work-dag-edge="informational:leaf->root"]')).not.toBeNull();
    expect(container.querySelector('[data-work-dag-edge="causal:side->leaf"]')).not.toBeNull();
    // Kind rides the end marker; the dash is the grade's, so the two
    // EXPLICIT kinds share one stroke and differ only by marker.
    const edge = (kind: string) => container.querySelector(`[data-work-dag-relation="${kind}"]`)!;
    expect(['gating', 'informational', 'causal'].map((kind) => edge(kind).getAttribute('data-work-dag-marker'))).toEqual([
      'arrowhead',
      'bar',
      'diamond',
    ]);
    expect(edge('gating').getAttribute('stroke-dasharray')).toBeNull();
    expect(edge('informational').getAttribute('stroke-dasharray')).toBe('6 2 1 2');
    expect(edge('causal').getAttribute('stroke-dasharray')).toBe(edge('informational').getAttribute('stroke-dasharray'));
  });

  it('emphasises the effort-weighted critical path the authority served, and only that', async () => {
    const container = await drawn();
    const critical = container.querySelector<HTMLInputElement>('[data-work-dag-critical]');
    expect(critical?.getAttribute('data-work-dag-critical')).toBe('on');
    expect(critical?.disabled).toBe(false);
    expect(card(container, 'root').getAttribute('data-work-dag-critical-node')).toBe('true');
    expect(card(container, 'middle').getAttribute('data-work-dag-critical-node')).toBe('true');
    expect(card(container, 'leaf').getAttribute('data-work-dag-critical-node')).toBe('true');
    expect(card(container, 'side').getAttribute('data-work-dag-critical-node')).toBeNull();
    expect(
      container.querySelector('[data-work-dag-edge="gating:root->middle"]')?.getAttribute('data-work-dag-emphasis'),
    ).toBe('alert');
    expect(
      container.querySelector('[data-work-dag-edge="gating:root->side"]')?.getAttribute('data-work-dag-emphasis'),
    ).toBe('edge');
  });

  it('disables the critical-path control with the authority\'s reason when the path is empty', async () => {
    serve(shape(workGraphRead({ ...GRAPH, criticalPath: [], criticalPathEffort: 0 })));
    const container = await drawn();
    const critical = container.querySelector<HTMLInputElement>('[data-work-dag-critical]');
    expect(critical?.disabled).toBe(true);
    expect(critical?.getAttribute('data-work-dag-critical')).toBe('absent');
    const note = critical?.getAttribute('aria-describedby');
    expect(note).not.toBeNull();
    expect(document.getElementById(note as string)?.textContent).toContain('empty critical path');
    expect(container.querySelector('[data-work-dag-critical-node]')).toBeNull();
  });

  /** Hover inspects. It isolates a neighbourhood and moves nothing else. */
  it('inspects a neighbourhood on hover without moving the selection', async () => {
    const container = await drawn();
    const before = address.search;

    fireEvent.pointerEnter(card(container, 'middle'));

    await waitFor(() =>
      expect(container.querySelector('[data-work-dag-inspected="middle"]')).not.toBeNull(),
    );
    expect(card(container, 'root').getAttribute('data-work-dag-card')).toBe('lit');
    expect(card(container, 'leaf').getAttribute('data-work-dag-card')).toBe('lit');
    expect(card(container, 'side').getAttribute('data-work-dag-card')).toBe('dimmed');
    expect(
      container.querySelector('[data-work-dag-edge="gating:root->middle"]')?.getAttribute('data-work-dag-emphasis'),
    ).toBe('accent');
    expect(
      container.querySelector('[data-work-dag-edge="gating:root->side"]')?.getAttribute('data-work-dag-emphasis'),
    ).toBe('dimmed');

    expect(address.search).toBe(before);
    expect(container.querySelector('[data-work-dag-card][aria-pressed="true"]')).toBeNull();
    expect(container.querySelector('[data-work-inspector="empty"]')).not.toBeNull();

    fireEvent.pointerLeave(container.querySelector('[data-work-dag-field]') as HTMLElement);
    await waitFor(() => expect(container.querySelector('[data-work-dag-inspected]')).toBeNull());
    expect(card(container, 'side').getAttribute('data-work-dag-card')).toBe('lit');
  });

  it('ends inspection when the pointer moves off a card onto the empty field', async () => {
    const container = await drawn();
    fireEvent.pointerEnter(card(container, 'middle'));
    await waitFor(() =>
      expect(container.querySelector('[data-work-dag-inspected="middle"]')).not.toBeNull(),
    );
    expect(card(container, 'side').getAttribute('data-work-dag-card')).toBe('dimmed');

    // The pointer stays inside the field, on the board's empty ground.
    fireEvent.pointerLeave(card(container, 'middle'), {
      relatedTarget: container.querySelector('[data-work-dag-fitted]'),
    });
    await waitFor(() => expect(container.querySelector('[data-work-dag-inspected]')).toBeNull());
    expect(card(container, 'side').getAttribute('data-work-dag-card')).toBe('lit');
  });

  it('traverses the graph with the arrow keys and selects with Enter', async () => {
    const user = userEvent.setup();
    const container = await drawn();

    // One roving tab stop: the first card when nothing is selected.
    expect(card(container, 'root').tabIndex).toBe(0);
    expect(card(container, 'leaf').tabIndex).toBe(-1);

    card(container, 'root').focus();
    await waitFor(() =>
      expect(container.querySelector('[data-work-dag-inspected="root"]')).not.toBeNull(),
    );
    await user.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(card(container, 'middle'));
    await user.keyboard('{ArrowRight}');
    expect(document.activeElement).toBe(card(container, 'side'));
    await user.keyboard('{ArrowLeft}');
    expect(document.activeElement).toBe(card(container, 'middle'));
    await user.keyboard('{ArrowUp}');
    expect(document.activeElement).toBe(card(container, 'root'));
    await user.keyboard('{ArrowDown}{ArrowDown}');
    expect(document.activeElement).toBe(card(container, 'leaf'));

    expect(address.search).not.toContain('task=');
    await user.keyboard('{Enter}');
    await waitFor(() => expect(card(container, 'leaf').getAttribute('aria-pressed')).toBe('true'));
    expect(address.search).toContain('task=leaf');
    expect(card(container, 'leaf').tabIndex).toBe(0);
    expect(card(container, 'root').tabIndex).toBe(-1);
    expect(container.querySelector('[data-work-inspector="leaf"]')).not.toBeNull();
  });

  it('isolates the declared path to root for the selected task', async () => {
    const user = userEvent.setup();
    const container = await drawn();
    const focus = screen.getByRole('combobox', { name: 'Focus path' }) as HTMLSelectElement;
    expect(focus.disabled).toBe(true);

    await user.click(card(container, 'middle'));
    await waitFor(() => expect(focus.disabled).toBe(false));
    await user.selectOptions(focus, 'root');
    fireEvent.pointerLeave(container.querySelector('[data-work-dag-field]') as HTMLElement);

    await waitFor(() => expect(card(container, 'leaf').getAttribute('data-work-dag-card')).toBe('dimmed'));
    expect(card(container, 'root').getAttribute('data-work-dag-card')).toBe('lit');
    expect(card(container, 'middle').getAttribute('data-work-dag-card')).toBe('lit');
    expect(card(container, 'side').getAttribute('data-work-dag-card')).toBe('dimmed');

    await user.selectOptions(focus, 'outcome');
    await waitFor(() => expect(card(container, 'leaf').getAttribute('data-work-dag-card')).toBe('lit'));
    expect(card(container, 'root').getAttribute('data-work-dag-card')).toBe('dimmed');
  });

  it('zooms the field only, in bounded steps, and fits back to the field', async () => {
    const user = userEvent.setup();
    const container = await drawn();
    const field = container.querySelector('[data-work-dag-field]');
    expect(field?.getAttribute('data-work-dag-zoom')).toBe('1.00');

    await user.click(screen.getByRole('button', { name: 'Zoom in' }));
    expect(field?.getAttribute('data-work-dag-zoom')).toBe('1.25');
    await user.click(screen.getByRole('button', { name: 'Zoom out' }));
    await user.click(screen.getByRole('button', { name: 'Zoom out' }));
    expect(field?.getAttribute('data-work-dag-zoom')).toBe('0.80');
    // Fit over an unmeasurable field (jsdom reports no width) returns to 100%
    // rather than to a guessed scale.
    await user.click(screen.getByRole('button', { name: 'Fit the graph to the field' }));
    expect(field?.getAttribute('data-work-dag-zoom')).toBe('1.00');

    // The controls and legend never scale with the field.
    expect(container.querySelector('[data-work-dag-controls]')?.getAttribute('style')).toBeNull();
  });

  it('withdraws titles but never identities when labels are hidden', async () => {
    const user = userEvent.setup();
    const container = await drawn();
    expect(card(container, 'middle').textContent).toContain('Middle task');
    await user.click(screen.getByRole('checkbox', { name: 'show labels' }));
    expect(card(container, 'middle').textContent).not.toContain('Middle task');
    expect(card(container, 'middle').textContent).toContain('middle');
    expect(card(container, 'middle').getAttribute('aria-label')).toContain('Middle task');
  });

  it('restates every card and relation in the exact table with the same selection control', async () => {
    const user = userEvent.setup();
    const container = await drawn();
    const table = container.querySelector<HTMLElement>('[data-work-dag-table]');
    expect(table).not.toBeNull();
    const tables = table?.querySelectorAll('table') ?? [];
    expect(tables).toHaveLength(2);
    for (const drawnTable of tables) {
      expect(drawnTable.querySelector('caption')?.textContent ?? '').not.toBe('');
      expect(drawnTable.querySelectorAll('th[scope="col"]').length).toBeGreaterThan(0);
    }
    expect(table?.querySelectorAll('[data-work-dag-row]')).toHaveLength(4);
    expect(table?.querySelectorAll('[data-work-dag-relation-row]')).toHaveLength(6);
    expect(within(table as HTMLElement).getByText('NO CARD')).toBeTruthy();

    const rowButton = table?.querySelector<HTMLButtonElement>('[data-work-dag-row="side"] [data-work-task="side"]');
    expect(rowButton?.className).toContain('min-h-[44px]');
    await user.click(rowButton as HTMLButtonElement);
    await waitFor(() => expect(card(container, 'side').getAttribute('aria-pressed')).toBe('true'));
  });

  it('draws an empty graph as the daemon\'s empty board, not a failed field', async () => {
    serve(workGraphRead({ ...GRAPH, tasks: [], criticalPath: [], criticalPathEffort: 0 }));
    const { container } = renderBoard();
    await waitFor(() =>
      expect(container.querySelector('[data-work-dag-board="empty"]')).not.toBeNull(),
    );
    expect(container.querySelector('[data-work-reading="empty"]')).not.toBeNull();
    expect(container.querySelector('[data-work-dag-card]')).toBeNull();
  });
});
