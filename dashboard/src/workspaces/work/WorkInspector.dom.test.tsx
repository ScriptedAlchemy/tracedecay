/**
 * The Work inspector over one selected task.
 *
 * The inspector separates definition, admission, relations, placement, and
 * evidence so no status stands in for another, and it prints the fields the
 * concept plate pictures but no Work authority publishes — priority, an owner
 * field — as the typed absences they are. Every row carries a grade.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../data/scope/store.ts';
import { workGraphRead, type WorkGraphVersionSpec } from '../../test/workGraphFixture.ts';
import { WorkPage } from './WorkPage.tsx';

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

const OBSERVED_AT = 1_800_000_000_000_000;

const GRAPH: WorkGraphVersionSpec = {
  version: 9,
  observedAt: OBSERVED_AT,
  tasks: [
    {
      taskId: 'root',
      title: 'Root task',
      effort: 2,
      lane: 'done',
      acceptedAt: OBSERVED_AT - 3_600_000_000,
      acceptedProposal: 'proposal.root',
      executionAdmittedAt: OBSERVED_AT - 7_200_000_000,
      handoffs: [
        {
          handoffId: 'handoff.1',
          fromActor: 'actor.planner',
          toActor: 'actor.builder',
          handedOffAt: OBSERVED_AT - 1_800_000_000,
        },
      ],
    },
    {
      taskId: 'leaf',
      title: 'Leaf task',
      effort: 5,
      dependencies: ['root'],
      causalCandidates: ['root'],
      lane: 'blocked',
      createdAt: OBSERVED_AT - 86_400_000_000,
      updatedAt: OBSERVED_AT - 60_000_000,
    },
  ],
  criticalPath: ['root', 'leaf'],
  runtimeAttempts: [
    { attemptId: 'attempt-1', taskId: 'leaf', runId: 'run-1', state: 'recovery_required' },
  ],
  runtimeCoverage: {
    coverage: 'partial',
    unavailable_attempts: [{ attempt_id: 'attempt-2', run_id: 'run-1', task_id: 'leaf' }],
  },
};

function serve(payload: unknown = workGraphRead(GRAPH)) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      const { status, body } = String(url).includes('/work/views')
        ? { status: 200, body: workEnvelope(payload, 'binding.http.work.views') }
        : { status: 503, body: { kind: 'problem', value: { problem: {} } } };
      return new Response(JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

function renderPage(entry: string) {
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

async function inspector(entry: string): Promise<HTMLElement> {
  const { container } = renderPage(entry);
  const aside = await waitFor(() => {
    const found = container.querySelector<HTMLElement>('[data-work-inspector]');
    if (found === null) throw new Error('inspector not drawn');
    return found;
  });
  return aside;
}

function row(scope: HTMLElement, term: string): HTMLElement {
  const dt = within(scope)
    .getAllByText(term, { selector: 'dt' })
    .at(0);
  if (dt === undefined) throw new Error(`no row ${term}`);
  return dt.parentElement as HTMLElement;
}

beforeEach(() => {
  serve();
});

afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('the Work inspector', () => {
  it('waits for a selection and says the inspector never mutates the graph', async () => {
    const aside = await inspector('/work?view=dag');
    expect(aside.getAttribute('data-work-inspector')).toBe('empty');
    expect(aside.textContent).toContain('never mutates the graph');
    // The evidence register is still drawn, over the graph as a whole.
    expect(aside.textContent).toContain('v9 / seq 12');
  });

  it('prints the selected task\'s lane, hierarchy, instants, and gates from the graph', async () => {
    const aside = await inspector('/work?view=dag&task=root');
    expect(aside.getAttribute('data-work-inspector')).toBe('root');
    expect(aside.querySelector('[data-work-inspector-lane="done"]')).not.toBeNull();

    const definition = aside.querySelector<HTMLElement>('[data-work-inspector-definition]');
    if (definition === null) throw new Error('no definition');
    expect(row(definition, 'milestone').textContent).toContain('milestone-1');
    expect(row(definition, 'effort').textContent).toContain('not a duration');
    expect(row(definition, 'created').textContent).toMatch(/\d{4}-\d{2}-\d{2}T/);
    expect(row(definition, 'scheduled').textContent).toContain('not scheduled');
    expect(row(definition, 'deadline').textContent).toContain('no deadline');

    const admission = aside.querySelector<HTMLElement>('[data-work-inspector-admission]');
    if (admission === null) throw new Error('no admission');
    expect(row(admission, 'proposal').textContent).toContain('proposal.root');
    expect(row(admission, 'accepted').textContent).toMatch(/\d{4}-\d{2}-\d{2}T/);
    expect(row(admission, 'admitted').textContent).toMatch(/\d{4}-\d{2}-\d{2}T/);
  });

  /** The assertion this file exists for: concept fields with no authority
   * are printed as absences, graded UNAVAILABLE, never filled from a neighbour. */
  it('states priority and owners as typed absences when no authority declares them', async () => {
    const aside = await inspector('/work?view=dag&task=leaf');
    const definition = aside.querySelector<HTMLElement>('[data-work-inspector-definition]');
    if (definition === null) throw new Error('no definition');

    const priority = row(definition, 'priority');
    expect(priority.textContent).toContain('declares no priority field');
    expect(priority.querySelector('[data-evidence-grade="unavailable"]')).not.toBeNull();

    const owners = row(definition, 'owners');
    expect(owners.textContent).toContain('no owner field');
    expect(owners.querySelector('[data-evidence-grade="unavailable"]')).not.toBeNull();
  });

  it('names handoff actors as explicit claims rather than as an owner field', async () => {
    const aside = await inspector('/work?view=dag&task=root');
    const definition = aside.querySelector<HTMLElement>('[data-work-inspector-definition]');
    if (definition === null) throw new Error('no definition');
    const actors = row(definition, 'actors');
    expect(actors.textContent).toContain('actor.planner, actor.builder');
    expect(actors.textContent).toContain('not an owner field');
    expect(actors.querySelector('[data-evidence-grade="explicit"]')).not.toBeNull();
    expect(within(definition).queryByText('owners', { selector: 'dt' })).toBeNull();
  });

  it('keeps placement, attempts, and topology metrics as separate typed readings', async () => {
    const aside = await inspector('/work?view=dag&task=leaf');
    const placement = aside.querySelector<HTMLElement>('[data-work-inspector-placement]');
    if (placement === null) throw new Error('no placement');
    expect(row(placement, 'placement').textContent).toContain('not read under this projection');
    expect(row(placement, 'placement').querySelector('[data-evidence-grade="unavailable"]')).not.toBeNull();

    const attempts = row(placement, 'attempts');
    expect(attempts.textContent).toContain('run-1 / attempt-1 · recovery_required');
    expect(attempts.textContent).toContain('a floor');
    expect(attempts.querySelector('[data-evidence-grade="ambiguous"]')).not.toBeNull();

    const evidence = aside.querySelector<HTMLElement>('[data-work-inspector-evidence]');
    if (evidence === null) throw new Error('no evidence');
    expect(row(evidence, 'topology metrics').textContent).toContain('not read under this projection');
    expect(row(evidence, 'graph version').textContent).toContain('v9 / seq 12');
    expect(row(evidence, 'selection scope').textContent).toContain('profile-owned');
    expect(row(evidence, 'attempts').textContent).toContain('1 unavailable, so a floor');
  });

  it('prints the kanban card\'s legal actions beside the prepared commands', async () => {
    const aside = await inspector('/work?view=dag&task=leaf');
    expect(screen.getByText(/Commands · Leaf task/)).toBeTruthy();
    const legal = aside.querySelector('[data-work-legal-actions]');
    expect(legal?.textContent).toContain('Prepared by the daemon');
    expect(legal?.textContent).toContain('kanban card lists no legal action');
    expect(screen.getByRole('button', { name: 'Accept task' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Admit execution' })).toBeTruthy();
  });

  it('selects a related task from the relations register', async () => {
    const aside = await inspector('/work?view=dag&task=leaf');
    const relations = aside.querySelector<HTMLElement>('[data-work-inspector-relations]');
    if (relations === null) throw new Error('no relations');
    const gatesOn = row(relations, 'gates on');
    const link = within(gatesOn).getByRole('button', { name: 'root' });
    link.click();
    await waitFor(() =>
      expect(document.querySelector('[data-work-inspector="root"]')).not.toBeNull(),
    );
  });
});
