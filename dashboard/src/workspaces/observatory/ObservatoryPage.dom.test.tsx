import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ObservatoryPage } from './ObservatoryPage.tsx';

/**
 * Store-telemetry rendering against the current `/api/storage/telemetry`
 * contract (src/dashboard/storage_telemetry_api.rs).
 *
 * The endpoint's honesty rules are the assertions here: an unconfigured budget
 * is a missing owner *setting* and never reads as unsupported or as a pass; a
 * first watermark is a real baseline and never reads as zero growth; a growth
 * delta is signed and always carries its since-daemon-start coverage verbatim;
 * and roles that share one database appear once, naming every role.
 */

const SETTING_KEY = 'sync.retention.v1 store_soft_budgets_bytes';
const COVERAGE =
  'since-daemon-start: bounded in-process watermark ring recorded on each telemetry sample, not a persisted historical series';

describe('ObservatoryPage store telemetry', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it('renders every budget and growth state honestly, and merges shared-file roles', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    // Shared store file: one card, both roles named.
    const shared = await screen.findByText('graph · memory (shared store file)');
    expect(shared).toBeTruthy();

    // Evaluated · within budget shows the real observed size and soft limit.
    expect(screen.getByText(/within budget · 204\.7 MiB of 512\.0 MiB soft limit/)).toBeTruthy();
    // Evaluated · over budget shows the real overage.
    expect(
      screen.getByText(/over budget · 704\.0 MiB of 512\.0 MiB soft limit · over by 192\.0 MiB/),
    ).toBeTruthy();

    // Unset: a missing setting, named exactly, and never "unsupported".
    const unsetRow = document.querySelector('[data-dimension-state="unset"]');
    expect(unsetRow?.textContent).toContain(`no budget configured · set ${SETTING_KEY}`);
    // The setting is a mono token, so a missing setting is structurally — not
    // only chromatically — distinct from an undetermined read.
    expect(unsetRow?.querySelector(`[data-setting-key="${SETTING_KEY}"]`)).toBeTruthy();
    expect(screen.queryByText(/budget.*unsupported/i)).toBeNull();

    // Unknown budget never renders as a pass.
    expect(screen.getAllByText('budget could not be determined').length).toBe(2);

    // Baseline is a real first sample, explicitly not zero growth.
    expect(
      screen.getByText(/first sample this daemon lifetime — not zero growth · 71\.1 MiB measured/),
    ).toBeTruthy();

    // Observed growth is signed, counts store watermarks, and the retired
    // per-table wording is gone.
    expect(
      screen.getByText(/\+6\.1 MiB over 12 store-size watermarks · 198\.6 MiB → 204\.7 MiB/),
    ).toBeTruthy();
    expect(screen.getByText(/−4\.0 MiB over 24 store-size watermarks/)).toBeTruthy();
    expect(screen.queryByText(/table samples/)).toBeNull();

    // The coverage sentence appears verbatim on every real growth read.
    expect(screen.getAllByText(COVERAGE).length).toBe(4);

    // A failed pragma read stays unknown on both dimensions, never zeroed.
    expect(screen.getByText('growth could not be determined')).toBeTruthy();
    expect(screen.getByText(/telemetry could not be determined for this store/)).toBeTruthy();
  });

  it('distinguishes an unset budget from an undetermined one in the rendered state', async () => {
    stubTelemetry(telemetryPayload());
    renderObservatory();

    await screen.findByText('graph · memory (shared store file)');
    const budgets = Array.from(
      document.querySelectorAll('[data-dimension="budget"]'),
    ).map((node) => ({
      state: node.getAttribute('data-dimension-state'),
      tone: node.getAttribute('data-dimension-tone'),
    }));
    expect(budgets.map((row) => row.state)).toEqual([
      'within_budget',
      'over_budget',
      'unset',
      'unknown',
      'unknown',
    ]);
    const unset = budgets.find((row) => row.state === 'unset');
    const unknown = budgets.find((row) => row.state === 'unknown');
    expect(unset?.tone).toBe('unset');
    expect(unknown?.tone).toBe('unknown');
    expect(unset?.tone).not.toBe(unknown?.tone);
  });

  it('renders the canonical Doctor storage family with typed kinds and provenance', async () => {
    stubTelemetry(telemetryPayload(), storageFindingsPayload());
    renderObservatory();

    expect((await screen.findAllByText('Over-budget stores')).length).toBeGreaterThan(0);
    for (const label of [
      'Orphan stores',
      'Stale branch databases',
      'Incident debris',
      'Retention backlog',
    ]) {
      expect(screen.getAllByText(label).length).toBeGreaterThan(0);
    }
    expect(document.querySelectorAll('[data-storage-finding-kind]')).toHaveLength(5);
    expect(
      document.querySelector('[data-storage-finding-kind="over_budget_store"]')?.textContent,
    ).toContain('Degraded');
    expect(
      document.querySelector('[data-storage-finding-kind="stale_branch_dbs"]')?.textContent,
    ).toContain('Stale');
    expect(screen.getByText('store size observed against soft budget')).toBeTruthy();
    expect(
      screen.getByText('storage.over_budget_store.sessions.db.observed-8388608b.overage-4194304b'),
    ).toBeTruthy();
    expect(
      screen.getAllByText('use-case.application.storage.retention-collect'),
    ).toHaveLength(2);
    expect(screen.queryByText(/requires:/)).toBeNull();
  });

  it('renders every finding producer source state without treating unset or partial as clean', async () => {
    stubTelemetry(telemetryPayload(), sourceStatusFindingsPayload());
    renderObservatory();

    expect(await screen.findByLabelText('Storage finding source status')).toBeTruthy();
    const statusFor = (kind: string) =>
      document.querySelector(`[data-storage-source-kind="${kind}"]`);

    expect(statusFor('over_budget_store')?.getAttribute('data-storage-source-state')).toBe('unset');
    expect(statusFor('over_budget_store')?.textContent).toContain(
      'No owner budget configured · sync.retention.v1 store_soft_budgets_bytes',
    );
    expect(statusFor('orphan_store')?.getAttribute('data-storage-source-state')).toBe('partial');
    expect(statusFor('stale_branch_dbs')?.getAttribute('data-storage-source-state')).toBe(
      'unsupported',
    );
    expect(statusFor('incident_debris_present')?.getAttribute('data-storage-source-state')).toBe(
      'real',
    );
    expect(statusFor('retention_backlog')?.getAttribute('data-storage-source-state')).toBe(
      'partial',
    );
    expect(screen.queryByText(/all storage checks clean/i)).toBeNull();
  });
});

function renderObservatory() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ObservatoryPage />
    </QueryClientProvider>,
  );
}

function stubTelemetry(
  payload: unknown,
  findingsPayload: unknown = emptyStorageFindingsPayload(),
) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === '/api/storage/telemetry') return jsonResponse(envelope(payload));
      if (url === '/api/storage/findings') {
        return jsonResponse(envelope(findingsPayload));
      }
      if (url === '/api/doctor/findings') {
        return jsonResponse(
          envelope({
            family_filter: null,
            entries: [],
            report_coverage: null,
            remediations: [],
            known_families: ['storage'],
            note: 'no admitted Doctor report source is available for this dashboard scope',
          }),
        );
      }
      throw new Error(`unexpected fetch ${url}`);
    }),
  );
}

function emptyStorageFindingsPayload() {
  return {
    family_filter: 'storage',
    entries: [],
    report_coverage: null,
    remediations: [],
    known_families: ['storage'],
    note: 'canonical Doctor storage family contained no entries',
    kind_statuses: sourceStatuses(),
  };
}

function storageFindingsPayload() {
  const operation = {
    retention: 'use-case.application.storage.retention-collect',
    orphan: 'use-case.application.storage.collect-orphan-store',
    branch: 'use-case.application.storage.branch-gc',
    debris: 'use-case.application.storage.quarantine-and-collect-debris',
  } as const;
  const entry = (
    storageKind:
      | 'over_budget_store'
      | 'orphan_store'
      | 'stale_branch_dbs'
      | 'incident_debris_present'
      | 'retention_backlog',
    state: 'degraded' | 'stale',
    reference: string,
    statement: string,
    owningOperation: string,
  ) => ({
    finding: {
      family: 'storage',
      state,
      evidence: [{ family: 'storage', reference }],
      coverage: { completeness: 'complete', statement },
      remediation: { owning_operation: owningOperation, kind: 'action' },
    },
    storage_kind: storageKind,
  });
  return {
    family_filter: 'storage',
    entries: [
      entry(
        'over_budget_store',
        'degraded',
        'storage.over_budget_store.sessions.db.observed-8388608b.overage-4194304b',
        'store size observed against soft budget',
        operation.retention,
      ),
      entry(
        'orphan_store',
        'degraded',
        'storage.orphan_store.orphan.db.age-86400000000us.size-1048576b',
        'store identity no longer resolves to a live repository root',
        operation.orphan,
      ),
      entry(
        'stale_branch_dbs',
        'stale',
        'storage.stale_branch_dbs.branch.db.branch-feature.size-2097152b',
        "branch-scoped store whose git ref is gone awaits lifecycle removal",
        operation.branch,
      ),
      entry(
        'incident_debris_present',
        'degraded',
        'storage.incident_debris_present.graph.db.count-2.bytes-3145728b',
        'quarantine-eligible incident artifacts present beside a live store',
        operation.debris,
      ),
      entry(
        'retention_backlog',
        'stale',
        'storage.retention_backlog.sessions.db.table-lcm_raw_messages.bytes-4194304b',
        'retention-eligible rows are past their window awaiting collection',
        operation.retention,
      ),
    ],
    report_coverage: {
      families: [{ family: 'storage', consultation: { status: 'consulted' } }],
      completeness: 'complete',
      statement: {
        completeness: 'complete',
        statement: 'storage retention and size authorities were consulted',
      },
    },
    remediations: [
      {
        operation: operation.retention,
        surface: 'storage_runtime',
        preview_available: true,
        action_confirmation: 'required',
        summary: 'collect retention-eligible rows or reclaim an over-budget store',
      },
      {
        operation: operation.orphan,
        surface: 'storage_runtime',
        preview_available: true,
        action_confirmation: 'required',
        summary: 'collect a store whose project identity no longer resolves',
      },
      {
        operation: operation.branch,
        surface: 'storage_runtime',
        preview_available: true,
        action_confirmation: 'required',
        summary: 'remove branch-scoped databases whose git refs are gone',
      },
      {
        operation: operation.debris,
        surface: 'storage_runtime',
        preview_available: true,
        action_confirmation: 'required',
        summary: 'quarantine and collect incident debris beside a live store',
      },
    ],
    known_families: [
      'advisory',
      'configuration',
      'storage_runtime',
      'storage',
      'language_server',
      'semantic_index',
      'observability',
    ],
    note: 'storage retention and size authorities were consulted',
    kind_statuses: sourceStatuses({
      over_budget_store: {
        state: 'partial',
        observed_entries: 2,
        reason: '2 stores evaluated; 1 unset; 2 undetermined',
      },
      orphan_store: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
      stale_branch_dbs: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
      incident_debris_present: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
      retention_backlog: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
    }),
  };
}

function sourceStatusFindingsPayload() {
  return {
    ...emptyStorageFindingsPayload(),
    kind_statuses: sourceStatuses({
      over_budget_store: {
        state: 'unset',
        observed_entries: 0,
        reason:
          'No owner budget configured · sync.retention.v1 store_soft_budgets_bytes',
      },
      orphan_store: {
        state: 'partial',
        observed_entries: 0,
        reason: 'the canonical report did not carry per-producer completion evidence',
      },
      stale_branch_dbs: {
        state: 'unsupported',
        observed_entries: 0,
        reason: 'no admitted Doctor report source is available for this dashboard scope',
      },
      incident_debris_present: {
        state: 'real',
        observed_entries: 1,
        reason: 'canonical Doctor producer returned observed evidence',
      },
      retention_backlog: {
        state: 'partial',
        observed_entries: 0,
        reason: 'retention watermark was stale',
      },
    }),
  };
}

type SourceStatus = {
  state: 'real' | 'unset' | 'partial' | 'unsupported';
  observed_entries: number;
  reason: string;
};

function sourceStatuses(overrides: Partial<Record<string, SourceStatus>> = {}) {
  return [
    'over_budget_store',
    'orphan_store',
    'stale_branch_dbs',
    'incident_debris_present',
    'retention_backlog',
  ].map((kind) => ({
    kind,
    ...(overrides[kind] ?? {
      state: 'unsupported',
      observed_entries: 0,
      reason: 'no admitted Doctor report source is available for this dashboard scope',
    }),
  }));
}

function telemetryPayload() {
  return {
    stores: [
      {
        store: 'graph.db',
        role: 'graph',
        roles: ['graph', 'memory'],
        path: '/project/.tracedecay/graph.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'graph.db',
            page_size_bytes: 4096,
            page_count: 52_400,
            freelist_pages: 1_280,
            observed_at: 100,
          },
        },
        total_bytes: 214_630_400,
        free_bytes: 5_242_880,
        free_page_ratio: 0.024,
        budget: {
          state: 'evaluated',
          evaluation: { state: 'within_budget', observed: 214_630_400, soft_limit: 536_870_912 },
          setting_key: SETTING_KEY,
          reason: 'evaluated against the owner-configured soft limit of 536870912 bytes',
        },
        growth: {
          state: 'observed',
          coverage: COVERAGE,
          first_measured_at: 1,
          last_measured_at: 100,
          sample_count: 12,
          first_total_bytes: 208_207_872,
          current_total_bytes: 214_630_400,
          growth_bytes: 6_422_528,
          samples: [
            { measured_at: 1, total_bytes: 208_207_872, free_bytes: 4_112_384 },
            { measured_at: 100, total_bytes: 214_630_400, free_bytes: 5_242_880 },
          ],
        },
      },
      {
        store: 'lcm.db',
        role: 'lcm',
        roles: ['lcm'],
        path: '/profile/lcm.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'lcm.db',
            page_size_bytes: 4096,
            page_count: 180_224,
            freelist_pages: 2_048,
            observed_at: 100,
          },
        },
        total_bytes: 738_197_504,
        free_bytes: 8_388_608,
        free_page_ratio: 0.011,
        budget: {
          state: 'evaluated',
          evaluation: {
            state: 'over_budget',
            observed: 738_197_504,
            soft_limit: 536_870_912,
            overage: 201_326_592,
          },
          setting_key: SETTING_KEY,
          reason: 'evaluated against the owner-configured soft limit of 536870912 bytes',
        },
        growth: {
          state: 'observed',
          coverage: COVERAGE,
          first_measured_at: 1,
          last_measured_at: 100,
          sample_count: 24,
          first_total_bytes: 742_391_808,
          current_total_bytes: 738_197_504,
          growth_bytes: -4_194_304,
          samples: [
            { measured_at: 1, total_bytes: 742_391_808, free_bytes: 12_582_912 },
            { measured_at: 100, total_bytes: 738_197_504, free_bytes: 8_388_608 },
          ],
        },
      },
      {
        store: 'savings.db',
        role: 'savings',
        roles: ['savings'],
        path: '/profile/savings.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'savings.db',
            page_size_bytes: 4096,
            page_count: 18_200,
            freelist_pages: 420,
            observed_at: 100,
          },
        },
        total_bytes: 74_547_200,
        free_bytes: 1_720_320,
        free_page_ratio: 0.023,
        budget: {
          state: 'unset',
          reason: 'no soft size budget is configured by the owner for this store',
          setting_key: SETTING_KEY,
        },
        growth: {
          state: 'baseline',
          coverage: COVERAGE,
          measured_at: 100,
          total_bytes: 74_547_200,
          reason: 'first watermark recorded in this daemon lifetime',
        },
      },
      {
        store: 'sessions.db',
        role: 'sessions',
        roles: ['sessions'],
        path: '/profile/sessions.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'sessions.db',
            page_size_bytes: 4096,
            page_count: 9_600,
            freelist_pages: 96,
            observed_at: 100,
          },
        },
        total_bytes: 39_321_600,
        free_bytes: 393_216,
        free_page_ratio: 0.01,
        budget: {
          state: 'unknown',
          reason: 'the resolved runtime configuration could not be read',
        },
        growth: {
          state: 'baseline',
          coverage: COVERAGE,
          measured_at: 100,
          total_bytes: 39_321_600,
          reason: 'first watermark recorded in this daemon lifetime',
        },
      },
      {
        store: 'incident.db',
        role: 'incident',
        roles: ['incident'],
        path: '/profile/incident.db',
        read: { kind: 'unknown', store: 'incident.db' },
        total_bytes: null,
        free_bytes: null,
        free_page_ratio: null,
        budget: {
          state: 'unknown',
          reason: 'no observed size sample, so a configured budget could not be evaluated',
        },
        growth: {
          state: 'unknown',
          reason: 'no watermark could be recorded because the store size read did not produce a sample',
        },
      },
    ],
    budget_note: 'budgets are owner configuration: sync.retention.v1 store_soft_budgets_bytes',
    growth_note: 'growth is measured over the watermarks this daemon has recorded since it started',
  };
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function envelope<T>(payload: T) {
  return {
    schema_revision: 1,
    scope: { project_id: 'project.observatory', storage_mode: 'project_local', store_root: '/p' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 100 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'partial',
      eligible: 5,
      examined: 4,
      matched: null,
      excluded: null,
      omitted: 1,
      unknown: null,
      denominator: 5,
      unit: 'stores',
      omission_reasons: ['store telemetry read failed (pragma unavailable)'],
    },
    freshness: { state: 'fresh', observed_at_micros: 100, watermark: null },
    domain_state: 'ready',
    legal_actions: [
      { kind: 'refresh', operation: 'use-case.dashboard.storage.telemetry.refresh' },
    ],
    payload,
  };
}
