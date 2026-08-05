import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { DoctorEvidenceStateV1Schema } from '../../contracts/generated.ts';
import { useScope } from '../../data/scope/store.ts';
import { DoctorInspector } from './DoctorInspector.tsx';

const EVIDENCE_STATES = DoctorEvidenceStateV1Schema.options.map((option) => option.value);

describe('DoctorInspector', () => {
  afterEach(() => {
    useScope.getState().selectAllProjects();
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
              known_families: ['configuration'],
              note: 'no admitted Doctor report source is available for this dashboard scope',
            },
            'unsupported',
          ),
        ),
      ),
    );

    renderDoctor();

    expect(
      (
        await screen.findAllByText(
          /no admitted Doctor report source is available for this dashboard scope/,
        )
      ).length,
    ).toBeGreaterThan(0);
    expect(screen.getAllByText('Unsupported').length).toBeGreaterThan(0);
    expect(screen.queryAllByRole('button', { name: /remedi|preview|apply/i })).toHaveLength(
      0,
    );
  });

  it('keeps every evidence label off its state hue while rendering each typed state', async () => {
    const response = findingsEnvelope();
    const template = response.payload.entries[0]!;
    response.payload.entries = EVIDENCE_STATES.map((state) => ({
      ...template,
      finding: {
        ...template.finding,
        state,
        coverage: {
          completeness: state === 'healthy_complete_coverage' ? 'complete' : 'partial',
          statement: `${state} evidence statement`,
        },
      },
    }));
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(response)));

    renderDoctor();
    const badges = await waitFor(() => {
      const found = document.querySelectorAll('[data-evidence-state]');
      expect(found).toHaveLength(EVIDENCE_STATES.length);
      return [...found];
    });

    for (const badge of badges) {
      const label =
        [...badge.querySelectorAll('span')].find(
          (span) =>
            span.querySelector('span') === null && (span.textContent ?? '').trim() !== '',
        ) ?? badge;
      for (let node: Element | null = label; node !== null; node = node.parentElement) {
        expect([...node.classList].filter((name) => name.startsWith('text-state-'))).toEqual(
          [],
        );
        if (node === badge) break;
      }
      expect([...label.classList]).toContain('text-text-secondary');
      expect(badge.querySelector('[aria-hidden][class*="bg-state-"]')).toBeTruthy();
      expect(badge.querySelector('svg[aria-hidden][class*="text-state-"]')).toBeTruthy();
    }
  });

  it('reads the selected project diagnosis without exposing a mutation journey', async () => {
    useScope.setState({
      scope: {
        kind: 'project',
        projectId: 'proj_other',
        label: 'Other project',
        activation: 'selected',
      },
    });
    const requests: Array<{ url: string; method: string }> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        requests.push({ url: String(input), method: init?.method ?? 'GET' });
        return jsonResponse(findingsEnvelope());
      }),
    );

    renderDoctor();
    await screen.findByText('configuration drift observed');

    expect(requests).toEqual([
      { url: '/api/projects/proj_other/doctor/findings', method: 'GET' },
    ]);
    expect(screen.queryAllByRole('button', { name: /remedi|preview|apply/i })).toHaveLength(
      0,
    );
  });
});

function renderDoctor() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
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

function envelope<T>(payload: T, state: 'partial' | 'unsupported') {
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
            completeness: 'partial',
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
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.doctor.findings.refresh' }],
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
          },
          storage_kind: null,
        },
      ],
      report_coverage: {
        families: [{ family: 'configuration', consultation: { status: 'consulted' } }],
        completeness: 'complete',
        statement: {
          completeness: 'complete',
          statement: 'configuration authority was consulted',
        },
      },
      known_families: ['configuration'],
      note: 'configuration drift observed',
    },
    'partial',
  );
}
