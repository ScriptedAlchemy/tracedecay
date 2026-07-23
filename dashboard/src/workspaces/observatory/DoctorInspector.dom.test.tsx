import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { DoctorInspector } from './DoctorInspector.tsx';
import { saveActiveDoctorOperation } from './doctorModel.ts';

const operation = 'use-case.application.configuration.protected-apply';

describe('DoctorInspector', () => {
  beforeEach(() => {
    window.localStorage.clear();
    vi.restoreAllMocks();
  });

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

    await user.click(screen.getByRole('button', { name: 'Review remediation' }));
    const apply = screen.getByRole('button', { name: 'Apply remediation' });
    expect((apply as HTMLButtonElement).disabled).toBe(true);

    await user.click(
      screen.getByRole('checkbox', {
        name: /I confirm this exact owner operation/,
      }),
    );
    expect((apply as HTMLButtonElement).disabled).toBe(false);
    await user.click(apply);

    expect(await screen.findByText(/denied/)).toBeTruthy();
    const applyCall = calls.find(({ url }) => url === '/api/doctor/remediations/apply');
    expect(JSON.parse(String(applyCall?.init?.body))).toMatchObject({
      operation,
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

  it('resumes the durable owner status identity after a reload', async () => {
    saveActiveDoctorOperation({
      schema_revision: 1,
      operation_id: 'request.doctor.resume',
      scope: {
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
      schema_revision: 1,
      operation_id: 'request.doctor.other-project',
      scope: {
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

function envelope(
  payload: unknown,
  state: string,
  legalActions: unknown[] = [],
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
  return envelope(
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

function operationEnvelope(phase: 'previewed' | 'failed', operationId: string) {
  return envelope(
    {
      status: 'operation',
      operation: {
        operation_id: operationId,
        owning_operation: operation,
        phase,
        preview_id: 'preview.doctor.preview',
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
      },
    },
    phase === 'failed' ? 'error' : 'ready',
  );
}
