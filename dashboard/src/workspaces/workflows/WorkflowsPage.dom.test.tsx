/** The Workflows page over the mounted `/application/workflow` routes: a
 * refusal is never an empty registry, and every rendered figure is a decoded
 * generated contract. */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../data/scope/store.ts';
import { WorkflowsPage } from './WorkflowsPage.tsx';

const DIGEST = `sha256:${'a'.repeat(64)}`;

function definition(version = 1) {
  return {
    definition_id: 'workflow.release-train',
    definition_version: version,
    project_id: 'project.workflows',
    steps: [
      {
        step_id: 'fan-out',
        operation: 'operation.work.start_attempt',
        predecessors: [],
        inputs: [],
        outputs: ['finding'],
        fan_out: { max_width: 3 },
      },
      {
        step_id: 'collect',
        operation: 'operation.work.synthesize',
        predecessors: ['fan-out'],
        inputs: [{ producer_step_id: 'fan-out', output_name: 'finding' }],
        outputs: [],
        fan_out: null,
      },
    ],
    pinned_policy_digest: DIGEST,
    pinned_configuration_digest: DIGEST,
    pinned_catalog_digest: DIGEST,
  };
}

function envelope(payload: unknown) {
  return {
    kind: 'success',
    value: {
      binding_id: 'binding.http.workflow.test',
      contract: { schema_id: 'schema.workflow.result', schema_revision: 1 },
      request_id: 'request-1',
      scope: {
        project_id: 'project.workflows',
        repository_id: 'repository.workflows',
        worktree_id: 'worktree.workflows',
        reference: null,
        scope_digest: 'sha256:scope',
      },
      outcome: { outcome: 'evidence', value: { payload } },
    },
  };
}

/** Answers exactly the routes a test names; anything else fails loudly. */
function serve(handler: (url: string, init?: RequestInit) => { status: number; body: unknown }) {
  const calls: { url: string; body: unknown }[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({
        url: String(url),
        body: typeof init?.body === 'string' ? JSON.parse(init.body) : undefined,
      });
      const { status, body } = handler(String(url), init);
      return new Response(JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
  return calls;
}

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <WorkflowsPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('the Workflows page over mounted routes', () => {
  it('lists registered definitions and opens the decoded step table', async () => {
    serve((url) =>
      url.includes('/application/workflow/list-definitions')
        ? { status: 200, body: envelope([definition()]) }
        : { status: 503, body: { kind: 'problem', value: { problem: {} } } },
    );
    renderPage();

    const row = await screen.findByRole('button', { name: /workflow\.release-train/ });
    expect(row.textContent).toContain('version 1');
    expect(row.textContent).toContain('2 steps');

    await userEvent.click(row);
    const detail = await screen.findByRole('region', {
      name: 'Definition workflow.release-train · version 1',
    });
    expect(detail.textContent).toContain('operation.work.start_attempt');
    expect(detail.textContent).toContain('operation.work.synthesize');
    expect(detail.textContent).toContain('max width 3');
    expect(detail.textContent).toContain('— entry step');
  });

  it('renders a refusal as the daemon’s own state, never as an empty registry', async () => {
    serve(() => ({ status: 503, body: { kind: 'problem', value: { problem: {} } } }));
    renderPage();

    expect(await screen.findByText(/the Work runtime is unavailable/)).toBeTruthy();
    expect(screen.queryByText(/no workflow definitions are registered/)).toBeNull();
    expect(document.querySelector('[data-workflow-definitions]')).toBeNull();
  });

  it('draws the empty registry only when the daemon answered one', async () => {
    serve((url) =>
      url.includes('/application/workflow/list-definitions')
        ? { status: 200, body: envelope([]) }
        : { status: 503, body: { kind: 'problem', value: { problem: {} } } },
    );
    renderPage();

    expect(
      await screen.findByText(
        /the daemon answered: no workflow definitions are registered in this scope/,
      ),
    ).toBeTruthy();
  });

  it('sends the compare-and-swap activation and renders the returned disposition', async () => {
    const calls = serve((url) => {
      if (url.includes('/application/workflow/list-definitions')) {
        return { status: 200, body: envelope([definition()]) };
      }
      if (url.includes('/application/workflow/activate-definition')) {
        return {
          status: 200,
          body: envelope({
            definition_id: 'workflow.release-train',
            definition_version: 1,
            state: 'active',
            revision: 3,
            transitioned_at: 10,
          }),
        };
      }
      return { status: 503, body: { kind: 'problem', value: { problem: {} } } };
    });
    renderPage();

    await userEvent.click(await screen.findByRole('button', { name: /workflow\.release-train/ }));
    await userEvent.click(await screen.findByRole('button', { name: 'activate' }));

    expect(await screen.findByText(/disposition active · revision 3/)).toBeTruthy();
    const activation = calls.find((call) =>
      call.url.includes('/application/workflow/activate-definition'),
    );
    expect(activation?.body).toEqual({
      definition_id: 'workflow.release-train',
      definition_version: 1,
      expected_revision: 1,
    });
  });

  it('resets the lifecycle controls when the operator switches definitions', async () => {
    const second = { ...definition(2), definition_id: 'workflow.nightly-sweep' };
    serve((url) => {
      if (url.includes('/application/workflow/list-definitions')) {
        return { status: 200, body: envelope([definition(), second]) };
      }
      if (url.includes('/application/workflow/activate-definition')) {
        return {
          status: 200,
          body: envelope({
            definition_id: 'workflow.release-train',
            definition_version: 1,
            state: 'active',
            revision: 3,
            transitioned_at: 10,
          }),
        };
      }
      return { status: 503, body: { kind: 'problem', value: { problem: {} } } };
    });
    renderPage();

    await userEvent.click(await screen.findByRole('button', { name: /workflow\.release-train/ }));
    await userEvent.click(await screen.findByRole('button', { name: 'activate' }));
    expect(await screen.findByText(/disposition active · revision 3/)).toBeTruthy();

    const draft = screen.getByLabelText('Expected revision');
    await userEvent.clear(draft);
    await userEvent.type(draft, '7');
    await userEvent.click(await screen.findByRole('button', { name: /workflow\.nightly-sweep/ }));

    expect(
      await screen.findByRole('region', { name: 'Definition workflow.nightly-sweep · version 2' }),
    ).toBeTruthy();
    expect(screen.queryByText(/disposition active · revision 3/)).toBeNull();
    expect((screen.getByLabelText('Expected revision') as HTMLInputElement).value).toBe('1');
  });

  it('refuses a lifecycle command under a read-only scope without dispatching it', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_other',
        label: 'Other project',
        activation: 'selected',
      },
    });
    const calls = serve((url) =>
      url.includes('/application/workflow/list-definitions')
        ? { status: 200, body: envelope([definition()]) }
        : { status: 503, body: { kind: 'problem', value: { problem: {} } } },
    );
    renderPage();

    await userEvent.click(await screen.findByRole('button', { name: /workflow\.release-train/ }));
    await userEvent.click(await screen.findByRole('button', { name: 'activate' }));

    // The scope authority's own reason, answered without a request: the
    // gateway serves every non-active project read-only.
    expect(await screen.findByText(/is not the active project/)).toBeTruthy();
    expect(
      calls.find((call) => call.url.includes('/application/workflow/activate-definition')),
    ).toBeUndefined();
    // The definitions read did dispatch, through the selected project's gateway.
    expect(
      calls.some((call) =>
        call.url.startsWith('/api/projects/proj_other/application/workflow/list-definitions'),
      ),
    ).toBe(true);
  });

  it('renders a lifecycle conflict verbatim rather than pretending the transition landed', async () => {
    serve((url) => {
      if (url.includes('/application/workflow/list-definitions')) {
        return { status: 200, body: envelope([definition()]) };
      }
      if (url.includes('/application/workflow/retire-definition')) {
        return { status: 409, body: { kind: 'problem', value: { problem: {} } } };
      }
      return { status: 503, body: { kind: 'problem', value: { problem: {} } } };
    });
    renderPage();

    await userEvent.click(await screen.findByRole('button', { name: /workflow\.release-train/ }));
    await userEvent.click(await screen.findByRole('button', { name: 'retire' }));

    expect(await screen.findByText(/the task moved since it was read/)).toBeTruthy();
    expect(screen.queryByText(/disposition \w+ · revision/)).toBeNull();
  });

  it('reads one run projection and renders its decoded step states', async () => {
    serve((url) => {
      if (url.includes('/application/workflow/list-definitions')) {
        return { status: 200, body: envelope([]) };
      }
      if (url.includes('/application/workflow/get-run')) {
        return {
          status: 200,
          body: envelope({
            run_id: 'run.release-train.1',
            definition: definition(),
            pinned_topology_digest: DIGEST,
            pinned_provider_registry_digest: DIGEST,
            status: 'running',
            sequence: 4,
            steps: {
              'fan-out': {
                status: 'running',
                outputs: {},
                placement_receipt: null,
                effect_receipt: null,
              },
              collect: {
                status: 'blocked',
                outputs: {},
                placement_receipt: null,
                effect_receipt: null,
              },
            },
            fan_out_plans: {},
            released_fan_out_attempts: [],
            settled_fan_out_attempts: [],
            history: [
              {
                run_id: 'run.release-train.1',
                sequence: 1,
                command_id: 'workflow-admit:1',
                input_digest: DIGEST,
                occurred_at: 5,
                event: {
                  type: 'admitted',
                  definition: definition(),
                  pinned_topology_digest: DIGEST,
                  pinned_provider_registry_digest: DIGEST,
                  fan_out_plans: [],
                },
              },
            ],
          }),
        };
      }
      return { status: 503, body: { kind: 'problem', value: { problem: {} } } };
    });
    renderPage();

    await userEvent.type(await screen.findByLabelText('Run id'), 'run.release-train.1');
    await userEvent.click(screen.getByRole('button', { name: 'Read run' }));

    await waitFor(() => {
      expect(document.querySelector('[data-workflow-run="run.release-train.1"]')).toBeTruthy();
    });
    expect(screen.getByText('status running')).toBeTruthy();
    expect(screen.getByText('sequence 4')).toBeTruthy();
    const fanOut = document.querySelector('[data-workflow-run-step="fan-out"]');
    expect(fanOut?.textContent).toContain('running');
    expect(fanOut?.textContent).toContain('no effect receipt yet');
  });

  it('refuses a run the daemon conceals rather than inventing an empty projection', async () => {
    serve((url) => {
      if (url.includes('/application/workflow/list-definitions')) {
        return { status: 200, body: envelope([]) };
      }
      if (url.includes('/application/workflow/get-run')) {
        return { status: 404, body: { kind: 'problem', value: { problem: {} } } };
      }
      return { status: 503, body: { kind: 'problem', value: { problem: {} } } };
    });
    renderPage();

    await userEvent.type(await screen.findByLabelText('Run id'), 'run.unknown');
    await userEvent.click(screen.getByRole('button', { name: 'Read run' }));

    expect(await screen.findByText(/not found, or not authorized for this actor/)).toBeTruthy();
    expect(document.querySelector('[data-workflow-run]')).toBeNull();
  });
});
