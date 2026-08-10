/**
 * The Work page over the exact product-graph authority.
 *
 * These replace the assertions that pinned the old absence ledger. That ledger
 * said no generated Work read model existed in the build, which `3f43664cb`
 * made false, so the assertions that held it in place have been rewritten as
 * behaviour over the real routes rather than deleted.
 *
 * The invariant they carry forward is the one that mattered: a refusal is never
 * a board. Every failure the daemon can return is asserted to render as its own
 * stated reason, and the empty-board case is asserted to be reachable *only*
 * from a product graph that actually said the board was empty.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../data/scope/store.ts';
import { workGraphRead } from '../../test/workGraphFixture.ts';
import { WorkPage } from './WorkPage.tsx';

function graphBody(
  tasks: Parameters<typeof workGraphRead>[0]['tasks'] = [
    { taskId: 'task-alpha', title: 'Alpha task' },
  ],
) {
  return {
    kind: 'success',
    value: {
      binding_id: 'binding.http.work.views',
      contract: { schema_id: 'schema.work.views.result', schema_revision: 1 },
      request_id: 'request-1',
      scope: {
        project_id: 'project.work',
        repository_id: 'repository.work',
        worktree_id: 'worktree.work',
        reference: null,
        scope_digest: 'sha256:scope',
      },
      outcome: {
        outcome: 'evidence',
        value: {
          payload: workGraphRead({ tasks }),
        },
      },
    },
  };
}

function graphBodyFromPayload(payload: ReturnType<typeof workGraphRead>) {
  const body = graphBody([]);
  body.value.outcome.value.payload = payload;
  return body;
}

function workSuccess(payload: unknown, bindingId: string) {
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

/** Answers the product graph route and refuses anything else, so a test that
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
  serve((url) =>
    url.includes('/work/views')
      ? { status: 200, body: graphBody() }
      : { status: 503, body: { kind: 'problem', value: { problem: {} } } },
  );
});

afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('the Work page over mounted routes', () => {
  it('draws the board from the product graph without calling the legacy snapshot', async () => {
    const calls: string[] = [];
    serve((url) => {
      calls.push(url);
      return url.includes('/work/views')
        ? { status: 200, body: graphBody() }
        : { status: 410, body: { kind: 'problem', value: { problem: {} } } };
    });

    renderPage();

    expect(await screen.findByRole('button', { name: 'Alpha task' })).toBeTruthy();
    expect(calls.some((url) => url.includes('/work/snapshot'))).toBe(false);
    expect(calls.some((url) => url.includes('/work/delta'))).toBe(false);
  });

  it('reads the product graph and names the task it returned', async () => {
    renderPage();
    expect(await screen.findByText('Alpha task')).toBeTruthy();
  });

  it('labels the default Work view as the active project', async () => {
    renderPage();
    expect(
      await screen.findByText('canonical task graph · the active project · exact product authority'),
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
      'canonical task graph · Beta (project-beta) · selected project · exact product authority',
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
        'canonical task graph · Alpha (project-alpha) · selected active project · exact product authority',
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
   * product graph of zero tasks are different facts, and the first must never be
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
  it('draws an empty board only from a current product graph that was empty', async () => {
    serve((url) =>
      url.includes('/work/views')
        ? { status: 200, body: graphBody([]) }
        : { status: 503, body: { kind: 'problem', value: { problem: {} } } },
    );
    const { container } = renderPage();
    await waitFor(() => expect(container.querySelector('[data-work-board]')).not.toBeNull());
    expect(screen.getAllByText(/No task in this build has reached this gate/).length).toBeGreaterThan(
      0,
    );
  });

  it('draws no boundary aside for the retired attempt family', async () => {
    renderPage();
    await screen.findByText('Alpha task');
    // Execution belongs to the Workflow runtime; there is no withheld attempt
    // inventory left to disclose, so the page must not print one.
    expect(screen.queryByLabelText('Work boundary')).toBeNull();
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

  it('accepts a task through the prepared product mutation and never calls the legacy command', async () => {
    const calls: string[] = [];
    const prepared = {
      mutation: 'accept_task',
      request: {
        evidence_by_criterion: {},
        mutation: {
          causation_event_id: null,
          command_id: 'command.accept-task',
          evidence: [],
          expected_authority: {
            authority: 'verified',
            verified_version: {
              event_sequence: 12,
              graph_version: 4,
              recovered_graph_digest: 'digest-graph',
              source_watermark: {},
            },
          },
          occurred_at: 1,
          revisions: {
            catalog_generation_id: 'catalog-1',
            configuration_revision_id: 'configuration-1',
            policy_revision_id: 'policy-1',
          },
        },
        selection: { selection: 'profile_owned_no_git' },
        task_id: 'task-alpha',
      },
    };
    serve((url) => {
      calls.push(url);
      if (url.includes('/work/views')) return { status: 200, body: graphBody() };
      if (url.includes('/work/prepare-graph-mutation')) {
        return {
          status: 200,
          body: workSuccess(prepared, 'binding.http.work.prepare_graph_mutation'),
        };
      }
      return { status: 503, body: { kind: 'problem', value: { problem: {} } } };
    });
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: 'Alpha task' }));

    await user.click(await screen.findByRole('button', { name: 'Accept task' }));

    await waitFor(() =>
      expect(calls.some((url) => url.includes('/work/mutate-graph'))).toBe(true),
    );
    expect(calls.some((url) => url.includes('/work/accept-task'))).toBe(false);
  });

  it('applies an accepted relation replan through the product mutation and never calls the legacy command', async () => {
    const calls: string[] = [];
    const graph = workGraphRead({
      version: 4,
      tasks: [
        { taskId: 'task-alpha', title: 'Alpha task' },
        { taskId: 'task-beta', title: 'Beta task' },
      ],
      relationReplanDecisions: [
        {
          decided_at: 1_799_999_999_000_000,
          disposition: 'accepted',
          proposal: {
            based_on_version: 3,
            causal_candidates: [],
            dependencies: ['task-beta'],
            informational_relations: [],
            payload_digest: 'digest.replan-alpha',
            proposal_id: 'proposal.replan-alpha',
            task_id: 'task-alpha',
          },
        },
      ],
    });
    const prepared = {
      mutation: 'apply_relation_replan',
      request: {
        mutation: {
          causation_event_id: null,
          command_id: 'command.apply-replan',
          evidence: [],
          expected_authority: {
            authority: 'verified',
            verified_version: {
              event_sequence: 12,
              graph_version: 4,
              recovered_graph_digest: 'digest-graph',
              source_watermark: {},
            },
          },
          occurred_at: 1_800_000_000_000_000,
          revisions: {
            catalog_generation_id: 'catalog-1',
            configuration_revision_id: 'configuration-1',
            policy_revision_id: 'policy-1',
          },
        },
        proposal_id: 'proposal.replan-alpha',
        selection: { selection: 'profile_owned_no_git' },
      },
    };
    serve((url) => {
      calls.push(url);
      if (url.includes('/work/views')) {
        return { status: 200, body: graphBodyFromPayload(graph) };
      }
      if (url.includes('/work/prepare-graph-mutation')) {
        return {
          status: 200,
          body: workSuccess(prepared, 'binding.http.work.prepare_graph_mutation'),
        };
      }
      return { status: 503, body: { kind: 'problem', value: { problem: {} } } };
    });
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: 'Alpha task' }));

    await user.click(await screen.findByRole('button', { name: 'Apply accepted relation replan' }));

    await waitFor(() =>
      expect(calls.some((url) => url.includes('/work/mutate-graph'))).toBe(true),
    );
    expect(calls.some((url) => url.includes('/work/replan-dependencies'))).toBe(false);
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
