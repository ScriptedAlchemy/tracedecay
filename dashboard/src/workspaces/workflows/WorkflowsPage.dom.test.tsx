/** The Workflows ledger over the mounted `/application/workflow` routes: a
 * refusal is never an empty registry, hover inspects without selecting, the
 * version track is its own read, lifecycle commands pass through an explicit
 * confirmation and render only the daemon's receipt, and run timing comes
 * from the run's own journal. */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../data/scope/store.ts';
import { WorkflowsPage } from './WorkflowsPage.tsx';

const DIGEST = `sha256:${'a'.repeat(64)}`;
const DIGEST_B = `sha256:${'b'.repeat(64)}`;

function definition(version = 1, overrides: Record<string, unknown> = {}) {
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
      {
        step_id: 'publish',
        operation: 'operation.work.publish',
        predecessors: ['collect'],
        inputs: [],
        outputs: ['artifact'],
        fan_out: null,
      },
    ],
    pinned_policy_digest: DIGEST,
    pinned_configuration_digest: DIGEST,
    pinned_catalog_digest: DIGEST,
    ...overrides,
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

const PROBLEM = { status: 503, body: { kind: 'problem', value: { problem: {} } } };

/** Answers exactly the routes a test names; anything else fails loudly. */
function serve(handler: (url: string, body: unknown) => { status: number; body: unknown }) {
  const calls: { url: string; body: unknown }[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      const body = typeof init?.body === 'string' ? JSON.parse(init.body) : undefined;
      calls.push({ url: String(url), body });
      const { status, body: response } = handler(String(url), body);
      return new Response(JSON.stringify(response), {
        status,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
  return calls;
}

/** A registry of one identity at two versions plus a second identity, with
 * the history route answering the selected identity's own track. */
function serveRegistry(extra?: (url: string, body: unknown) => { status: number; body: unknown } | null) {
  const v1 = definition(1);
  const v2 = definition(2, { pinned_policy_digest: DIGEST_B });
  const nightly = definition(1, {
    definition_id: 'workflow.nightly-sweep',
    steps: definition().steps.slice(0, 1),
  });
  return serve((url, body) => {
    const handled = extra?.(url, body);
    if (handled) return handled;
    if (url.includes('/application/workflow/list-definitions')) {
      return { status: 200, body: envelope([v1, v2, nightly]) };
    }
    if (url.includes('/application/workflow/definition-history')) {
      const id = (body as { definition_id: string }).definition_id;
      return {
        status: 200,
        body: envelope(id === 'workflow.release-train' ? [v1, v2] : [nightly]),
      };
    }
    return PROBLEM;
  });
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

function registryRow(name: RegExp) {
  return screen.findByRole('button', { name });
}

afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('the Workflows registry', () => {
  it('folds every served version under its identity and reports both counts', async () => {
    serveRegistry();
    renderPage();

    const row = await registryRow(/workflow\.release-train/);
    // Latest version, version count, step count, and an unread disposition.
    expect(row.textContent).toContain('v2');
    expect(row.textContent).toContain('unread');
    expect(row.getAttribute('aria-pressed')).toBe('false');
    expect(document.querySelector('[data-workflow-definitions]')?.getAttribute('data-workflow-definitions')).toBe('2');
    expect(document.querySelector('[data-workflow-versions]')?.getAttribute('data-workflow-versions')).toBe('3');
    expect(screen.getByTestId('workflow-registry-count').textContent).toBe(
      '2 definitions · 3 versions',
    );
  });

  it('renders a refusal as the daemon's own state, never as an empty registry', async () => {
    serve(() => PROBLEM);
    renderPage();

    expect(await screen.findByText(/the Work runtime is unavailable/)).toBeTruthy();
    expect(screen.queryByText(/no workflow definitions are registered/)).toBeNull();
    expect(document.querySelector('[data-workflow-definitions]')).toBeNull();
    expect(screen.getByTestId('workflow-registry-count').textContent).toBe('registry unread');
  });

  it('draws the empty registry only when the daemon answered one', async () => {
    serve((url) =>
      url.includes('/application/workflow/list-definitions')
        ? { status: 200, body: envelope([]) }
        : PROBLEM,
    );
    renderPage();

    expect(
      await screen.findByText(
        /the daemon answered: no workflow definitions are registered in this scope/,
      ),
    ).toBeTruthy();
    expect(screen.getByText(/nothing to select/)).toBeTruthy();
  });

  it('filters on identity and says how many remain registered', async () => {
    serveRegistry();
    renderPage();
    await registryRow(/workflow\.release-train/);

    await userEvent.type(screen.getByLabelText('Filter by identity'), 'nightly');
    expect(screen.queryByRole('button', { name: /workflow\.release-train/ })).toBeNull();
    expect(screen.getByRole('button', { name: /workflow\.nightly-sweep/ })).toBeTruthy();
    expect(screen.getByTestId('workflow-registry-count').textContent).toContain('1 shown');

    await userEvent.clear(screen.getByLabelText('Filter by identity'));
    await userEvent.type(screen.getByLabelText('Filter by identity'), 'zzz');
    expect(screen.getByTestId('workflow-registry-no-match').textContent).toContain(
      '2 remain registered',
    );
  });

  it('inspects on hover and focus without changing the selection', async () => {
    serveRegistry();
    renderPage();
    const train = await registryRow(/workflow\.release-train/);
    const nightly = screen.getByRole('button', { name: /workflow\.nightly-sweep/ });

    expect(screen.getByTestId('workflow-inspect-mode').textContent).toBe('idle');

    await userEvent.hover(nightly);
    expect(screen.getByTestId('workflow-inspect-mode').textContent).toBe('hover · inspect only');
    expect(document.querySelector('[data-workflow-inspect="workflow.nightly-sweep"]')).toBeTruthy();
    // Hover is not selection: nothing is pressed and no detail is decoded.
    expect(nightly.getAttribute('aria-pressed')).toBe('false');
    expect(document.querySelector('[data-workflow-selected]')).toBeNull();

    await userEvent.click(train);
    expect(train.getAttribute('aria-pressed')).toBe('true');
    await userEvent.unhover(nightly);
    expect(screen.getByTestId('workflow-inspect-mode').textContent).toBe('selection');
    expect(document.querySelector('[data-workflow-inspect="workflow.release-train"]')).toBeTruthy();

    // Keyboard focus inspects too, so nothing is hover-only.
    act(() => nightly.focus());
    expect(document.querySelector('[data-workflow-inspect="workflow.nightly-sweep"]')).toBeTruthy();
    expect(train.getAttribute('aria-pressed')).toBe('true');
  });

  it('roves focus over rows with the arrow keys', async () => {
    serveRegistry();
    renderPage();
    const nightly = await registryRow(/workflow\.nightly-sweep/);
    const train = screen.getByRole('button', { name: /workflow\.release-train/ });

    nightly.focus();
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(train);
    await userEvent.keyboard('{ArrowUp}');
    expect(document.activeElement).toBe(nightly);
    await userEvent.keyboard('{End}');
    expect(document.activeElement).toBe(train);
  });
});

describe('the selected definition', () => {
  it('decodes the latest version, names the plate fields the contract lacks, and reads the version track', async () => {
    const calls = serveRegistry();
    renderPage();

    await userEvent.click(await registryRow(/workflow\.release-train/));
    const detail = await screen.findByRole('region', { name: 'Selected definition' });
    expect(detail.querySelector('[data-workflow-selected]')?.getAttribute('data-workflow-selected')).toBe(
      'workflow.release-train@2',
    );
    expect(detail.textContent).toContain(DIGEST_B);
    // Typed absences, not blank cells.
    for (const field of ['created / updated', 'description', 'step timeout / retries']) {
      expect(detail.querySelector(`[data-absence="${field}"]`)?.textContent).toContain('UNAVAILABLE');
    }

    const track = await screen.findByRole('region', { name: /Version track/ });
    await waitFor(() => {
      expect(track.querySelector('[data-workflow-version-track]')?.getAttribute('data-workflow-version-track')).toBe('2');
    });
    const history = calls.find((call) => call.url.includes('/application/workflow/definition-history'));
    expect(history?.body).toEqual({ definition_id: 'workflow.release-train' });
    const v2 = track.querySelector('[data-workflow-version="2"]')!;
    expect(v2.querySelector('[data-pin-delta="changed"]')).toBeTruthy();
    expect(v2.querySelectorAll('[data-pin-delta="same"]')).toHaveLength(2);
    const v1 = track.querySelector('[data-workflow-version="1"]')!;
    expect(v1.querySelectorAll('[data-pin-delta="first"]')).toHaveLength(3);

    const steps = screen.getByRole('region', { name: /Decoded step table · v2/ });
    expect(steps.textContent).toContain('operation.work.start_attempt');
    expect(steps.textContent).toContain('fan-out.finding');
    expect(steps.textContent).toContain('max width 3');
    expect(steps.textContent).toContain('— entry step');
  });

  it('selects an earlier version from the track and re-keys the decoded steps', async () => {
    serveRegistry();
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));
    const track = await screen.findByRole('region', { name: /Version track/ });
    await userEvent.click(await within(track).findByRole('button', { name: 'v1' }));

    expect(
      document.querySelector('[data-workflow-selected]')?.getAttribute('data-workflow-selected'),
    ).toBe('workflow.release-train@1');
    expect(screen.getByRole('region', { name: /Decoded step table · v1/ })).toBeTruthy();
    expect(within(track).getByRole('button', { name: 'v1' }).getAttribute('aria-pressed')).toBe('true');
  });

  it('renders a refused version track as its own typed state beside a decoded definition', async () => {
    serveRegistry((url) =>
      url.includes('/application/workflow/definition-history')
        ? { status: 404, body: { kind: 'problem', value: { problem: {} } } }
        : null,
    );
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));

    const track = await screen.findByRole('region', { name: /Version track/ });
    expect(await within(track).findByText(/not found, or not authorized/)).toBeTruthy();
    expect(track.querySelector('[data-workflow-version-track]')).toBeNull();
    // The definition itself still decoded from the registry.
    expect(document.querySelector('[data-workflow-selected]')).toBeTruthy();
  });

  it('lights a step's neighbours on hover and pins them on click', async () => {
    serveRegistry();
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));
    const steps = await screen.findByRole('region', { name: /Decoded step table/ });
    const collect = within(steps).getByRole('button', { name: 'collect' });

    await userEvent.hover(collect);
    expect(steps.querySelector('[data-workflow-step="collect"]')?.getAttribute('data-step-relation')).toBe('self');
    expect(steps.querySelector('[data-workflow-step="fan-out"]')?.getAttribute('data-step-relation')).toBe('upstream');
    expect(steps.querySelector('[data-workflow-step="publish"]')?.getAttribute('data-step-relation')).toBe('downstream');
    expect(screen.getByTestId('workflow-step-inspect').textContent).toBe(
      'collect · 1 upstream · 1 downstream',
    );

    await userEvent.unhover(collect);
    expect(steps.querySelector('[data-workflow-step="collect"]')?.getAttribute('data-step-relation')).toBe('none');

    await userEvent.click(collect);
    expect(collect.getAttribute('aria-pressed')).toBe('true');
    await userEvent.unhover(collect);
    expect(screen.getByTestId('workflow-step-inspect').textContent).toContain('pinned');
  });
});

describe('lifecycle compare-and-swap', () => {
  function disposition(state: string, revision: number) {
    return envelope({
      definition_id: 'workflow.release-train',
      definition_version: 2,
      state,
      revision,
      transitioned_at: 1_746_807_242_000_000,
    });
  }

  it('confirms before sending, sends the exact body, and renders only the returned disposition', async () => {
    const calls = serveRegistry((url) =>
      url.includes('/application/workflow/activate-definition')
        ? { status: 200, body: disposition('active', 3) }
        : null,
    );
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));

    const result = screen.getByTestId('workflow-lifecycle-result');
    expect(result.textContent).toContain('No lifecycle command has been sent');

    await userEvent.click(screen.getByRole('button', { name: /^Activate v2/ }));
    const dialog = await screen.findByRole('dialog', {
      name: 'Confirm activate · workflow.release-train v2',
    });
    expect(dialog.textContent).toContain('operation.workflow.activate_definition');
    expect(within(dialog).getByTestId('workflow-confirm-scope').textContent).toContain('writable');
    // Nothing dispatched by opening the dialog.
    expect(calls.some((call) => call.url.includes('activate-definition'))).toBe(false);

    const send = within(dialog).getByRole('button', { name: 'Send activate' });
    expect((send as HTMLButtonElement).disabled).toBe(true);
    await userEvent.click(
      within(dialog).getByRole('checkbox', {
        name: /I confirm this compare-and-swap against expected revision 1/,
      }),
    );
    await userEvent.click(send);

    await waitFor(() => {
      expect(result.querySelector('[data-lifecycle-receipt="active"]')).toBeTruthy();
    });
    expect(result.textContent).toContain('disposition active');
    expect(result.textContent).toContain('revision 3');
    expect(result.textContent).toContain('2025-05-09 16:14:02');
    expect(result.textContent).toContain('CAS RECEIPT');
    const activation = calls.find((call) => call.url.includes('/application/workflow/activate-definition'));
    expect(activation?.body).toEqual({
      definition_id: 'workflow.release-train',
      definition_version: 2,
      expected_revision: 1,
    });
    // The receipt reaches the registry row and the version track for that exact version.
    const row = screen.getByRole('button', { name: /workflow\.release-train/ });
    expect(row.querySelector('[data-disposition="active"]')).toBeTruthy();
    const track = screen.getByRole('region', { name: /Version track/ });
    expect(track.querySelector('[data-workflow-version="2"] [data-disposition="active"]')).toBeTruthy();
    expect(track.querySelector('[data-workflow-version="1"] [data-disposition="unread"]')).toBeTruthy();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('cancelling the confirmation sends nothing', async () => {
    const calls = serveRegistry();
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));
    await userEvent.click(screen.getByRole('button', { name: /^Retire v2/ }));
    const dialog = await screen.findByRole('dialog');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));

    expect(screen.queryByRole('dialog')).toBeNull();
    expect(calls.some((call) => call.url.includes('retire-definition'))).toBe(false);
    expect(screen.getByTestId('workflow-lifecycle-result').textContent).toContain(
      'No lifecycle command has been sent',
    );
  });

  it('resets the staged revision and last result when the operator switches versions', async () => {
    serveRegistry((url) =>
      url.includes('/application/workflow/activate-definition')
        ? { status: 200, body: disposition('active', 3) }
        : null,
    );
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));

    const draft = screen.getByLabelText(/Expected revision/);
    await userEvent.clear(draft);
    await userEvent.type(draft, '7');
    await userEvent.click(screen.getByRole('button', { name: /^Activate v2/ }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog.textContent).toContain('expected revision 7');
    await userEvent.click(within(dialog).getByRole('checkbox'));
    await userEvent.click(within(dialog).getByRole('button', { name: 'Send activate' }));
    await waitFor(() => {
      expect(screen.getByTestId('workflow-lifecycle-result').textContent).toContain('revision 3');
    });

    await userEvent.click(screen.getByRole('button', { name: /workflow\.nightly-sweep/ }));
    expect(
      document.querySelector('[data-workflow-selected]')?.getAttribute('data-workflow-selected'),
    ).toBe('workflow.nightly-sweep@1');
    expect(screen.getByTestId('workflow-lifecycle-result').textContent).toContain(
      'No lifecycle command has been sent',
    );
    expect((screen.getByLabelText(/Expected revision/) as HTMLInputElement).value).toBe('1');
  });

  it('fails closed under a read-only scope: the reason is shown and nothing dispatches', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_other',
        label: 'Other project',
        activation: 'selected',
      },
    });
    const calls = serveRegistry();
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));
    expect(screen.getByTestId('workflow-scope-writability').textContent).toBe('scope read only');

    await userEvent.click(screen.getByRole('button', { name: /^Activate v2/ }));
    const dialog = await screen.findByRole('dialog');
    // The scope authority's own reason: the gateway serves every non-active project read-only.
    expect(within(dialog).getByTestId('workflow-confirm-scope').textContent).toContain(
      'is not the active project',
    );
    expect((within(dialog).getByRole('checkbox') as HTMLInputElement).disabled).toBe(true);
    expect((within(dialog).getByRole('button', { name: 'Send activate' }) as HTMLButtonElement).disabled).toBe(true);
    expect(calls.find((call) => call.url.includes('/application/workflow/activate-definition'))).toBeUndefined();
    // The reads did dispatch, through the selected project's gateway.
    expect(
      calls.some((call) =>
        call.url.startsWith('/api/projects/proj_other/application/workflow/list-definitions'),
      ),
    ).toBe(true);
    expect(
      calls.some((call) =>
        call.url.startsWith('/api/projects/proj_other/application/workflow/definition-history'),
      ),
    ).toBe(true);
  });

  it('renders a conflict verbatim rather than pretending the transition landed', async () => {
    serveRegistry((url) =>
      url.includes('/application/workflow/retire-definition')
        ? { status: 409, body: { kind: 'problem', value: { problem: {} } } }
        : null,
    );
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));
    await userEvent.click(screen.getByRole('button', { name: /^Retire v2/ }));
    const dialog = await screen.findByRole('dialog');
    await userEvent.click(within(dialog).getByRole('checkbox'));
    await userEvent.click(within(dialog).getByRole('button', { name: 'Send retire' }));

    const result = screen.getByTestId('workflow-lifecycle-result');
    expect(await within(result).findByText(/the task moved since it was read/)).toBeTruthy();
    expect(result.textContent).toContain('did not transition anything');
    expect(result.querySelector('[data-lifecycle-receipt]')).toBeNull();
    expect(
      screen.getByRole('button', { name: /workflow\.release-train/ }).querySelector('[data-disposition="unread"]'),
    ).toBeTruthy();
  });

  it('refuses to stage a command on an invalid expected revision', async () => {
    serveRegistry();
    renderPage();
    await userEvent.click(await registryRow(/workflow\.release-train/));
    const draft = screen.getByLabelText(/Expected revision/);
    await userEvent.clear(draft);
    await userEvent.type(draft, '0');

    expect(screen.getByRole('alert').textContent).toContain('at least 1');
    expect((screen.getByRole('button', { name: /^Activate v2/ }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe('exact run lookup', () => {
  function placement(stepId: string) {
    return {
      backend: 'codex_cli',
      configuration_digest: DIGEST,
      model: 'gpt-5',
      placement_digest: DIGEST,
      provider_registry_digest: DIGEST,
      route: { provider_id: 'provider.codex', route_id: 'route.default' },
      run_id: 'run.release-train.1',
      step_id: stepId,
      topology_digest: DIGEST,
      worktree_placement: { kind: 'repository_local_root' },
    };
  }
  function receipt(stepId: string, outcome: string) {
    return {
      effect_digest: DIGEST,
      outcome,
      output_set_digest: DIGEST,
      placement_digest: DIGEST,
      receipt_digest: DIGEST,
      run_id: 'run.release-train.1',
      step_id: stepId,
    };
  }
  function event(sequence: number, occurredAt: number, kind: Record<string, unknown>) {
    return {
      run_id: 'run.release-train.1',
      sequence,
      command_id: `workflow-command:${sequence}`,
      input_digest: DIGEST,
      occurred_at: occurredAt,
      event: kind,
    };
  }
  const T0 = 1_746_807_242_000_000;
  function projection(status: string) {
    const pinned = definition(2, { pinned_policy_digest: DIGEST_B });
    return {
      run_id: 'run.release-train.1',
      definition: pinned,
      pinned_topology_digest: DIGEST,
      pinned_provider_registry_digest: DIGEST,
      status,
      sequence: 4,
      steps: {
        'fan-out': {
          status: 'succeeded',
          outputs: { finding: { output_name: 'finding', artifacts: [] } },
          placement_receipt: placement('fan-out'),
          effect_receipt: receipt('fan-out', 'completed'),
        },
        collect: { status: 'running', outputs: {}, placement_receipt: placement('collect'), effect_receipt: null },
        publish: { status: 'blocked', outputs: {}, placement_receipt: null, effect_receipt: null },
      },
      fan_out_plans: {},
      released_fan_out_attempts: [],
      settled_fan_out_attempts: [],
      history: [
        event(1, T0, {
          type: 'admitted',
          definition: pinned,
          pinned_topology_digest: DIGEST,
          pinned_provider_registry_digest: DIGEST,
          fan_out_plans: [],
        }),
        event(2, T0 + 19_000_000, { type: 'step_started', step_id: 'fan-out', placement: placement('fan-out') }),
        event(3, T0 + 40_000_000, {
          type: 'step_completed',
          step_id: 'fan-out',
          outputs: [],
          effect_receipt: receipt('fan-out', 'completed'),
        }),
        event(4, T0 + 336_000_000, { type: 'step_started', step_id: 'collect', placement: placement('collect') }),
      ],
    };
  }

  it('reads one run, derives its timing from the journal, and refuses a duration for a live run', async () => {
    serveRegistry((url) =>
      url.includes('/application/workflow/get-run')
        ? { status: 200, body: envelope(projection('running')) }
        : null,
    );
    renderPage();
    expect(screen.getByTestId('workflow-run-state').textContent).toBe('idle');

    await userEvent.type(await screen.findByLabelText('Run id'), 'run.release-train.1');
    await userEvent.click(screen.getByRole('button', { name: 'Read run' }));
    await waitFor(() => {
      expect(document.querySelector('[data-workflow-run="run.release-train.1"]')).toBeTruthy();
    });
    expect(screen.getByTestId('workflow-run-state').textContent).toBe('loaded');

    const run = document.querySelector('[data-workflow-run]')!;
    expect(run.querySelector('[data-run-status="running"]')).toBeTruthy();
    expect(run.textContent).toContain('2025-05-09 16:14:02');
    expect(screen.getByTestId('workflow-run-span').textContent).toBe('00:05:36');
    expect(run.textContent).toContain('elapsed to last event');
    expect(run.textContent).toContain('not finished, so not a duration');
    expect(run.textContent).toContain('1 / 3');
    expect(run.textContent).toContain('4 journal events');

    const fanOut = run.querySelector('[data-workflow-run-step="fan-out"]')!;
    expect(fanOut.getAttribute('data-step-status')).toBe('succeeded');
    expect(fanOut.textContent).toContain('2025-05-09 16:14:21');
    expect(fanOut.textContent).toContain('00:00:21');
    expect(fanOut.textContent).toContain('codex_cli');
    expect(fanOut.textContent).toContain('gpt-5');
    expect(fanOut.textContent).toContain('effect completed');
    const collect = run.querySelector('[data-workflow-run-step="collect"]')!;
    expect(collect.textContent).toContain('not settled');
    expect(collect.textContent).toContain('no effect receipt');
    const publish = run.querySelector('[data-workflow-run-step="publish"]')!;
    expect(publish.textContent).toContain('not started');
    expect(publish.textContent).toContain('no placement receipt');
    expect(run.querySelector('[data-workflow-run-journal]')?.getAttribute('data-workflow-run-journal')).toBe('4');
  });

  it('gives a finished run a duration and pivots to its pinned definition when the registry lists it', async () => {
    serveRegistry((url) =>
      url.includes('/application/workflow/get-run')
        ? { status: 200, body: envelope(projection('completed')) }
        : null,
    );
    renderPage();
    await registryRow(/workflow\.release-train/);
    await userEvent.type(screen.getByLabelText('Run id'), 'run.release-train.1');
    await userEvent.click(screen.getByRole('button', { name: 'Read run' }));
    await waitFor(() => {
      expect(document.querySelector('[data-workflow-run]')).toBeTruthy();
    });
    const run = document.querySelector('[data-workflow-run]')!;
    expect(run.textContent).toContain('duration');
    expect(run.textContent).not.toContain('elapsed to last event');
    expect(screen.getByTestId('workflow-run-span').textContent).toBe('00:05:36');

    await userEvent.click(within(run as HTMLElement).getByRole('button', { name: 'select in registry' }));
    expect(
      document.querySelector('[data-workflow-selected]')?.getAttribute('data-workflow-selected'),
    ).toBe('workflow.release-train@2');
  });

  it('marks a pinned definition the registry does not list instead of inventing a selection', async () => {
    serve((url) => {
      if (url.includes('/application/workflow/list-definitions')) {
        return { status: 200, body: envelope([]) };
      }
      if (url.includes('/application/workflow/get-run')) {
        return { status: 200, body: envelope(projection('running')) };
      }
      return PROBLEM;
    });
    renderPage();
    await userEvent.type(await screen.findByLabelText('Run id'), 'run.release-train.1');
    await userEvent.click(screen.getByRole('button', { name: 'Read run' }));
    await waitFor(() => {
      expect(document.querySelector('[data-workflow-run]')).toBeTruthy();
    });
    expect(screen.queryByRole('button', { name: 'select in registry' })).toBeNull();
    expect(document.querySelector('[data-workflow-run]')?.textContent).toContain(
      'not in the loaded registry',
    );
  });

  it('refuses a run the daemon conceals rather than inventing an empty projection', async () => {
    serve((url) => {
      if (url.includes('/application/workflow/list-definitions')) {
        return { status: 200, body: envelope([]) };
      }
      if (url.includes('/application/workflow/get-run')) {
        return { status: 404, body: { kind: 'problem', value: { problem: {} } } };
      }
      return PROBLEM;
    });
    renderPage();

    await userEvent.type(await screen.findByLabelText('Run id'), 'run.unknown');
    await userEvent.click(screen.getByRole('button', { name: 'Read run' }));

    expect(await screen.findByText(/not found, or not authorized for this actor/)).toBeTruthy();
    expect(screen.getByTestId('workflow-run-state').textContent).toBe('denied');
    expect(document.querySelector('[data-workflow-run]')).toBeNull();
    expect(screen.getByText(/is not an empty run/)).toBeTruthy();
  });
});
