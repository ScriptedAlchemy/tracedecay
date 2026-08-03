import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DoctorEvidenceStateV1Schema } from '../../contracts/generated.ts';
import { useScope } from '../../data/scope/store.ts';
import { DoctorInspector } from './DoctorInspector.tsx';
import { saveActiveDoctorOperation } from './doctorModel.ts';

const operation = 'use-case.application.configuration.pin-authority';

/** Every `DoctorEvidenceStateV1`, so the badge check covers the whole taxonomy
 * rather than the states one fixture happens to carry. */
const EVIDENCE_STATES = DoctorEvidenceStateV1Schema.options.map((option) => option.value);

describe('DoctorInspector', () => {
  beforeEach(() => {
    window.localStorage.clear();
    vi.restoreAllMocks();
  });

  afterEach(() => useScope.getState().selectAllProjects());

  it('renders an unavailable Doctor source as unsupported, never empty or healthy', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        jsonResponse(
          envelope(
            {
              family_filter: null,
              entries: [],
              report_coverage: null,
              remediations: [],
              known_families: [
                'advisory',
                'configuration',
                'storage_runtime',
                'storage',
                'language_server',
                'semantic_index',
                'observability',
              ],
              note: 'no admitted Doctor report source is available for this dashboard scope',
            },
            'unsupported',
          ),
        ),
      ),
    );

    renderDoctor();

    expect(
      await screen.findByText(
        /no admitted Doctor report source is available for this dashboard scope/,
      ),
    ).toBeTruthy();
    expect(screen.getAllByText('Unsupported').length).toBeGreaterThan(0);
    expect(screen.queryByText(/zero findings/i)).toBeNull();
  });

  /**
   * The evidence badge follows the `StateChip` rule: the state hue rides the
   * lamp and the glyph, and the label text stays on an AA-contrast token.
   *
   * This is structural rather than a numeric contrast check because jsdom
   * computes no colours — but it fails on exactly the markup that produced the
   * violation. The badge used to put `evidence.tokenClass` on the span carrying
   * its own label, and colour inherits, so a hue anywhere from the label up to
   * the badge root is the defect. Measured on `--surface-2` at the 11px the
   * label renders at, five of these tokens miss WCAG AA 4.5:1 — light:
   * partial 3.88, stale 4.06, ready 4.21; dark: error 4.41, unknown 4.44 — so
   * every state is checked rather than a sample.
   */
  it('keeps every evidence badge label off the state hue, and puts the hue on the lamp and icon', async () => {
    const response = findingsEnvelope();
    const template = response.payload.entries[0]!;
    response.payload.entries = EVIDENCE_STATES.map((state) => ({
      ...template,
      finding: {
        ...template.finding,
        state,
        // Only a healthy finding may claim complete coverage of a healthy
        // result, so the rest are downgraded to keep the entry wire-legal.
        coverage: {
          completeness: state === 'healthy_complete_coverage' ? 'complete' : 'partial',
          statement: `${state} evidence statement`,
        },
      },
    }));
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(response)));

    renderDoctor();
    await screen.findByLabelText('Doctor diagnosis');
    const badges = await waitFor(() => {
      const found = document.querySelectorAll('[data-evidence-state]');
      expect(found).toHaveLength(EVIDENCE_STATES.length);
      return [...found];
    });

    for (const badge of badges) {
      const state = badge.getAttribute('data-evidence-state');
      // The innermost element carrying the label text — or the badge itself
      // when the label is a bare child of it, which is how the defective
      // version rendered and is exactly the case that must reach the
      // inherited-hue assertion below rather than stopping short of it.
      const label =
        [...badge.querySelectorAll('span')].find(
          (span) => span.querySelector('span') === null && (span.textContent ?? '').trim() !== '',
        ) ?? badge;

      // Colour inherits, so the hue must be absent from the label and from
      // every element between it and the badge root.
      for (let node: Element | null = label; node !== null; node = node.parentElement) {
        expect(
          [...node.classList].filter((name) => name.startsWith('text-state-')),
          `the ${state} badge label inherits a state hue from ${node.className}`,
        ).toEqual([]);
        if (node === badge) break;
      }
      expect([...label.classList]).toContain('text-text-secondary');

      // The hue is not lost: it is on the decorative lamp and glyph, both of
      // which are hidden from assistive technology because the label carries
      // the meaning.
      const lamp = badge.querySelector('[aria-hidden][class*="bg-state-"]');
      expect(lamp, `the ${state} badge lost its lamp`).toBeTruthy();
      const icon = badge.querySelector('svg[aria-hidden][class*="text-state-"]');
      expect(icon, `the ${state} badge lost its state glyph`).toBeTruthy();
    }
  });

  it('renders per-family denied and unknown coverage instead of hiding it in aggregate state', async () => {
    const response = findingsEnvelope();
    response.payload.report_coverage!.families.push(
      {
        family: 'semantic_index',
        consultation: { status: 'unavailable', reason: 'denied' },
      },
      {
        family: 'language_server',
        consultation: { status: 'unavailable', reason: 'unknown' },
      },
    );
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(response)));

    renderDoctor();

    expect(await screen.findByLabelText('Doctor source coverage gaps')).toBeTruthy();
    expect(screen.getByText(/Semantic index denied/)).toBeTruthy();
    expect(screen.getByText(/Language server unknown/)).toBeTruthy();
  });

  it('previews and explicitly confirms only the exact server-authorized operation', async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        calls.push({ url, init });
        if (url === '/api/doctor/findings') {
          return jsonResponse(findingsEnvelope());
        }
        if (url === '/api/doctor/remediations/preview') {
          return jsonResponse(operationEnvelope('previewed', 'request.doctor.preview'));
        }
        if (url === '/api/doctor/remediations/request.doctor.preview') {
          return jsonResponse(operationEnvelope('previewed', 'request.doctor.preview'));
        }
        if (url === '/api/doctor/remediations/apply') {
          return jsonResponse(
            envelope(
              { status: 'unavailable', reason: 'denied' },
              'denied',
            ),
          );
        }
        throw new Error(`unexpected fetch ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderDoctor();

    await user.click(await screen.findByRole('button', { name: 'Preview' }));
    expect(await screen.findByText('Remediation previewed')).toBeTruthy();
    const previewCall = calls.find(({ url }) => url === '/api/doctor/remediations/preview');
    expect(JSON.parse(String(previewCall?.init?.body))).toMatchObject({
      operation,
      target: { owner_operation: 'configuration_pin_authority' },
    });

    await user.click(screen.getByRole('button', { name: 'Review remediation' }));
    expect(
      screen.getByText(/authority scope will be resolved and rechecked by the owner/),
    ).toBeTruthy();
    const apply = screen.getByRole('button', { name: 'Apply remediation' });
    expect((apply as HTMLButtonElement).disabled).toBe(true);

    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this exact owner operation/,
      }),
    );
    expect((apply as HTMLButtonElement).disabled).toBe(false);
    await user.click(apply);

    expect((await screen.findAllByText(/denied/)).length).toBeGreaterThan(0);
    const applyCall = calls.find(({ url }) => url === '/api/doctor/remediations/apply');
    expect(JSON.parse(String(applyCall?.init?.body))).toMatchObject({
      operation,
      target: { owner_operation: 'configuration_pin_authority' },
      preview_id: 'preview.doctor.preview',
      confirmed: true,
    });

    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this exact owner operation/,
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Apply remediation' }));
    await waitFor(() => {
      expect(
        calls.filter(({ url }) => url === '/api/doctor/remediations/apply'),
      ).toHaveLength(2);
    });
    const applyBodies = calls
      .filter(({ url }) => url === '/api/doctor/remediations/apply')
      .map(({ init }) => JSON.parse(String(init?.body)));
    expect(applyBodies[1]?.idempotency_key).toBe(applyBodies[0]?.idempotency_key);
  });

  it('allows a legal direct action while leaving authority resolution to the owner', async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        calls.push({ url, init });
        if (url === '/api/doctor/findings') {
          return jsonResponse(directActionFindingsEnvelope());
        }
        if (url === '/api/doctor/remediations/apply') {
          return jsonResponse(envelope({ status: 'unavailable', reason: 'denied' }, 'denied'));
        }
        throw new Error(`unexpected fetch ${url}`);
      }),
    );
    const user = userEvent.setup();
    renderDoctor();

    await user.click(await screen.findByRole('button', { name: 'Review remediation' }));
    expect(
      screen.getByText(/authority scope will be resolved and rechecked by the owner/),
    ).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Preview' })).toBeNull();

    const apply = screen.getByRole('button', { name: 'Apply remediation' });
    expect((apply as HTMLButtonElement).disabled).toBe(true);
    await user.click(
      screen.getByRole('checkbox', {
        name: /owner must resolve and recheck authority/,
      }),
    );
    expect((apply as HTMLButtonElement).disabled).toBe(false);
    await user.click(apply);

    expect((await screen.findAllByText(/denied/)).length).toBeGreaterThan(0);
    const applyCall = calls.find(({ url }) => url === '/api/doctor/remediations/apply');
    expect(JSON.parse(String(applyCall?.init?.body))).toMatchObject({
      operation,
      target: { owner_operation: 'configuration_pin_authority' },
      preview_id: null,
      confirmed: true,
    });
  });

  it('names the owning surface when an authorized action is not addressable here', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(ownerSuppliedChangeFindingsEnvelope())),
    );

    renderDoctor();

    expect(
      await screen.findByText(
        /Authorized by the configuration control plane, which also supplies the exact change to apply/,
      ),
    ).toBeTruthy();
    // The owner authorized the apply, so the card must not claim otherwise.
    expect(
      screen.queryByText(/No authorized remediation action is currently available/),
    ).toBeNull();
    // ...and it must not offer a control it cannot send a change with.
    expect(screen.queryByRole('button', { name: 'Review remediation' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Preview' })).toBeNull();
  });

  it('reports an owner that authorizes nothing as exactly that', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        const response = findingsEnvelope();
        response.legal_actions = response.legal_actions.filter(
          (action: { kind: string }) => action.kind === 'refresh',
        );
        return jsonResponse(response);
      }),
    );

    renderDoctor();

    expect(
      await screen.findByText(/No authorized remediation action is currently available/),
    ).toBeTruthy();
  });

  it('resumes the durable owner status identity after a reload', async () => {
    saveActiveDoctorOperation({
      schema_revision: 3,
      operation_id: 'request.doctor.resume',
      transport_scope: {
        project_id: 'project.doctor-test',
        storage_mode: 'project_local',
        store_root: '/project',
      },
    });
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        calls.push(url);
        if (url === '/api/doctor/findings') {
          return jsonResponse(
            envelope(
              {
                family_filter: null,
                entries: [],
                report_coverage: null,
                remediations: [],
                known_families: [],
                note: 'Doctor report unavailable while remediation status remains resumable',
              },
              'unsupported',
            ),
          );
        }
        if (url === '/api/doctor/remediations/request.doctor.resume') {
          return jsonResponse(operationEnvelope('failed', 'request.doctor.resume'));
        }
        throw new Error(`unexpected fetch ${url}`);
      }),
    );

    renderDoctor();

    expect(await screen.findByText('Remediation failed')).toBeTruthy();
    expect(screen.getByText('request.doctor.resume')).toBeTruthy();
    expect(calls).toContain('/api/doctor/remediations/request.doctor.resume');
  });

  it('does not query a saved operation through a different project scope', async () => {
    saveActiveDoctorOperation({
      schema_revision: 3,
      operation_id: 'request.doctor.other-project',
      transport_scope: {
        project_id: 'project.other',
        storage_mode: 'project_local',
        store_root: '/other',
      },
    });
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        calls.push(url);
        if (url === '/api/doctor/findings') {
          return jsonResponse(findingsEnvelope());
        }
        throw new Error(`cross-scope status request ${url}`);
      }),
    );

    renderDoctor();

    expect(
      await screen.findByText(/saved remediation belongs to a different dashboard scope/),
    ).toBeTruthy();
    expect(calls).toEqual(['/api/doctor/findings']);
  });

  /**
   * Doctor reads through the project gateway like everything around it.
   *
   * It used to request `/api/doctor/findings` unprefixed, which the daemon
   * answers for whichever project *it* has active. Selecting a project therefore
   * put one project's diagnosis in this panel and another's in the storage
   * telemetry beside it and the health dot in the rail, and nothing on screen
   * said which project the findings belonged to.
   */
  it('reads findings in the selected scope rather than the daemon-active project', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_other',
        label: 'Other project',
        activation: 'selected',
      },
    });
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        calls.push(String(input));
        return jsonResponse(findingsEnvelope());
      }),
    );

    renderDoctor();
    await screen.findByLabelText('Doctor diagnosis');

    await waitFor(() => expect(calls.length).toBeGreaterThan(0));
    expect(calls).toEqual(['/api/projects/proj_other/doctor/findings']);
  });

  /**
   * The gateway serves a non-active project's reads and refuses its writes, so
   * a selected project shows its diagnosis with its controls disabled. Enabling
   * them would offer a remediation the daemon will refuse — on a control whose
   * whole purpose is to mutate a store.
   */
  it('shows a selected project’s findings with its remediation controls disabled', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_other',
        label: 'Other project',
        activation: 'selected',
      },
    });
    const methods: Array<string> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
        methods.push(init?.method ?? 'GET');
        return jsonResponse(findingsEnvelope());
      }),
    );

    renderDoctor();

    // The finding itself is on screen: a read-only scope is not a reason to
    // withhold the diagnosis.
    expect(await screen.findByText('configuration drift observed')).toBeTruthy();
    const preview = screen.getByRole<HTMLButtonElement>('button', { name: /Preview/ });
    const review = screen.getByRole<HTMLButtonElement>('button', {
      name: /Review remediation/,
    });
    expect(preview.disabled).toBe(true);
    expect(review.disabled).toBe(true);

    const note = document.querySelector('[data-scope-writability]');
    expect(note?.getAttribute('data-scope-writability')).toBe('read_only');
    expect(note?.textContent).toContain('Switch scope to the active project');

    // A preview is a POST, so it is a write to the gateway and must not have
    // been sent — the disabled button is not the only guard, but nothing tried.
    expect(methods.filter((method) => method !== 'GET')).toEqual([]);
  });

  it('names the target of a remediation the active scope does accept', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_active',
        label: 'Active project',
        activation: 'active',
      },
    });
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(findingsEnvelope())));

    renderDoctor();
    await screen.findByText('configuration drift observed');

    expect(
      screen.getByRole<HTMLButtonElement>('button', { name: /Preview/ }).disabled,
    ).toBe(false);
    const note = document.querySelector('[data-scope-writability]');
    expect(note?.getAttribute('data-scope-writability')).toBe('writable');
    expect(note?.textContent).toBe('Remediations apply to Active project.');
  });
});

function renderDoctor() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });
  return render(
    <QueryClientProvider client={client}>
      <DoctorInspector />
    </QueryClientProvider>,
  );
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function envelope<T>(
  payload: T,
  state: string,
  legalActions: Array<{ kind: string; operation?: string }> = [],
) {
  return {
    schema_revision: 1,
    scope: {
      project_id: 'project.doctor-test',
      storage_mode: 'project_local',
      store_root: '/project',
    },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 100 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage:
      state === 'unsupported'
        ? {
            completeness: 'unsupported',
            eligible: null,
            examined: null,
            matched: null,
            excluded: null,
            omitted: null,
            unknown: null,
            denominator: null,
            unit: null,
            omission_reasons: [],
          }
        : {
            completeness: state === 'partial' ? 'partial' : 'complete',
            eligible: 1,
            examined: 1,
            matched: 1,
            excluded: 0,
            omitted: 0,
            unknown: 0,
            denominator: 1,
            unit: 'doctor_findings',
            omission_reasons: [],
          },
    freshness: {
      state: state === 'unsupported' ? 'unsupported' : 'fresh',
      observed_at_micros: 100,
      watermark: null,
    },
    domain_state: state,
    legal_actions: legalActions,
    payload,
  };
}

function findingsEnvelope() {
  return envelope<import('../../contracts/generated.ts').DoctorFindingsPayloadV1>(
    {
      family_filter: null,
      entries: [
        {
          finding: {
            family: 'configuration',
            state: 'degraded',
            evidence: [
              {
                family: 'configuration',
                reference: 'configuration:desired-effective-drift',
              },
            ],
            coverage: {
              completeness: 'complete',
              statement: 'configuration authority was consulted',
            },
            remediation: {
              owning_operation: operation,
              kind: 'action',
            },
          },
          storage_kind: null,
        },
      ],
      report_coverage: {
        families: [
          {
            family: 'configuration',
            consultation: { status: 'consulted' },
          },
        ],
        completeness: 'complete',
        statement: {
          completeness: 'complete',
          statement: 'configuration authority was consulted',
        },
      },
      remediations: [
        {
          operation,
          surface: 'configuration_control_plane',
          preview_available: true,
          action_confirmation: 'required',
          summary: 'apply the admitted configuration revision',
          target: { owner_operation: 'configuration_pin_authority' },
        },
      ],
      known_families: ['configuration'],
      note: 'configuration drift observed',
    },
    'partial',
    [
      { kind: 'refresh', operation: 'use-case.dashboard.doctor.findings.refresh' },
      { kind: 'request_dry_run', operation },
      { kind: 'request_apply', operation },
    ],
  );
}

/** A protected configuration apply: the owner authorizes it, but the findings
 * route cannot name the key, value, and base revision it would carry, so it
 * sends no target. */
function ownerSuppliedChangeFindingsEnvelope() {
  const protectedApply = 'use-case.application.configuration.protected-apply';
  const result = findingsEnvelope();
  result.payload.entries[0]!.finding.remediation!.owning_operation = protectedApply;
  result.payload.remediations[0]!.operation = protectedApply;
  result.payload.remediations[0]!.preview_available = false;
  result.payload.remediations[0]!.target = null;
  result.legal_actions = [
    { kind: 'refresh', operation: 'use-case.dashboard.doctor.findings.refresh' },
    { kind: 'request_apply', operation: protectedApply },
  ];
  return result;
}

function directActionFindingsEnvelope() {
  const result = findingsEnvelope();
  result.payload.remediations[0]!.preview_available = false;
  result.legal_actions = result.legal_actions.filter(
    (action: { kind: string }) => action.kind !== 'request_dry_run',
  );
  return result;
}

function operationEnvelope(
  phase: 'previewed' | 'failed',
  operationId: string,
) {
  return envelope(
    {
      status: 'operation',
      operation: {
        operation_id: operationId,
        owning_operation: operation,
        phase,
        preview_id: 'preview.doctor.preview',
        idempotency_key:
          phase === 'previewed' ? null : 'idempotency.dashboard-doctor.fixture',
        execution: {
          started_at: 1,
          ended_at: 2,
          effective_deadline: { expires_at: 10 },
          cancellation: null,
          budget: {
            units_consumed: 1,
            bytes_consumed: 0,
            elapsed_micros: 1,
          },
          termination: phase === 'failed' ? 'failed' : 'completed',
        },
        effect_receipt: null,
        verification:
          phase === 'previewed' ? { state: 'not_required' } : { state: 'unavailable' },
      },
    },
    phase === 'failed' ? 'error' : 'ready',
  );
}
