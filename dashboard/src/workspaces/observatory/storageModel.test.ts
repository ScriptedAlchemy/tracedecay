import { describe, expect, it } from 'vitest';
import {
  DoctorFindingsPayloadV1Schema,
  DashboardEnvelopeV1Schema,
  DashboardLegalActionRefV1Schema,
  StorageTelemetryPayloadV1Schema,
} from '../../contracts/generated.ts';
import { doctorEvidencePresentation } from './doctorModel.ts';
import {
  budgetPresentation,
  dimensionDotClass,
  formatSignedBytes,
  growthPresentation,
  refreshOperation,
  storageFindingLabel,
  storeRolesLabel,
  tableGrowthOmissionPresentation,
  tableGrowthPresentation,
} from './storageModel.ts';

const SETTING_KEY = 'sync.retention.v1 store_soft_budgets_bytes';

describe('Observatory storage read models', () => {
  it('renders refresh only from a server-supplied legal action reference', () => {
    expect(
      refreshOperation([
        {
          kind: 'refresh',
          operation: 'use-case.dashboard.storage.findings.refresh',
        },
      ]),
    ).toBe('use-case.dashboard.storage.findings.refresh');
    expect(
      refreshOperation([
        {
          kind: 'request_apply',
          operation: 'use-case.dashboard.storage.retention.apply',
        },
      ]),
    ).toBeUndefined();
    expect(
      DashboardLegalActionRefV1Schema.safeParse({
        kind: 'invented_action',
        operation: 'not-an-authority',
      }).success,
    ).toBe(false);
  });

  it('rejects an incompatible envelope revision instead of rendering it healthy', () => {
    const envelope = {
      schema_revision: 2,
      scope: { project_id: null, storage_mode: 'project_local', store_root: '/profile' },
      version: { entity_version: null, graph_version: null },
      time: { valid_time_micros: null, observation_time_micros: 123 },
      source_watermark: null,
      authorization: { outcome: 'authorized' },
      coverage: {
        completeness: 'complete',
        eligible: 0,
        examined: 0,
        matched: 0,
        excluded: 0,
        omitted: 0,
        unknown: 0,
        denominator: 0,
        unit: 'stores',
        omission_reasons: [],
      },
      freshness: { state: 'fresh', observed_at_micros: 123, watermark: null },
      domain_state: 'ready',
      legal_actions: [],
      payload: { stores: [], budget_note: '', growth_note: '' },
    };

    expect(DashboardEnvelopeV1Schema(StorageTelemetryPayloadV1Schema).safeParse(envelope).success).toBe(false);
  });

  it('decodes the canonical Doctor storage-family projection and maps typed kinds', () => {
    const payload = DoctorFindingsPayloadV1Schema.parse({
      family_filter: 'storage',
      entries: [
        {
          finding: {
            family: 'storage',
            state: 'degraded',
            evidence: [
              {
                family: 'storage',
                reference:
                  'storage.over_budget_store.sessions.db.observed-8388608b.overage-4194304b',
              },
            ],
            coverage: {
              completeness: 'complete',
              statement: 'store size observed against soft budget',
            },
            remediation: {
              owning_operation: 'use-case.application.storage.retention-collect',
              kind: 'action',
            },
          },
          storage_kind: 'over_budget_store',
        },
      ],
      report_coverage: {
        families: [{ family: 'storage', consultation: { status: 'consulted' } }],
        completeness: 'complete',
        statement: {
          completeness: 'complete',
          statement: 'storage retention and size authorities were consulted',
        },
      },
      remediations: [],
      known_families: ['storage'],
      note: 'storage retention and size authorities were consulted',
    });

    expect(
      (
        [
          'over_budget_store',
          'orphan_store',
          'stale_branch_dbs',
          'incident_debris_present',
          'retention_backlog',
          'table_growth',
        ] as const
      ).map(storageFindingLabel),
    ).toEqual([
      'Over-budget stores',
      'Orphan stores',
      'Stale branch databases',
      'Incident debris',
      'Retention backlog',
      'Table growth',
    ]);
    expect(payload.entries[0]?.storage_kind).toBe('over_budget_store');
    expect(payload.entries[0]?.finding.coverage.statement).toBe(
      'store size observed against soft budget',
    );
    expect(doctorEvidencePresentation(payload.entries[0]!.finding.state)).toEqual({
      label: 'Degraded',
      tokenClass: 'text-state-error',
      dotClass: 'bg-state-error',
      domainState: 'error',
    });
  });

  it('preserves the typed budget/growth dimensions and the roles a store serves', () => {
    const payload = StorageTelemetryPayloadV1Schema.parse({
      stores: [
        {
          store: 'graph.db',
          role: 'graph',
          roles: ['graph', 'memory'],
          path: '/profile/graph.db',
          read: {
            kind: 'observed',
            sample: {
              store: 'graph.db',
              page_size_bytes: 4096,
              page_count: 8,
              freelist_pages: 2,
              observed_at: 123,
            },
          },
          total_bytes: 32768,
          free_bytes: 8192,
          free_page_ratio: 0.25,
          budget: { state: 'unset', reason: 'no configured budget', setting_key: SETTING_KEY },
          growth: {
            state: 'unknown',
            reason: 'no execution-owned store-size watermark is available',
          },
          table_growth: {
            state: 'baseline_established',
            coverage: {
              completeness: 'partial',
              eligible: 1,
              examined: 0,
              matched: null,
              excluded: null,
              omitted: 1,
              unknown: null,
              denominator: 1,
              unit: 'store_table_growth_reads',
              omission_reasons: ['no baseline yet'],
            },
            observed_at: 123,
            tables_observed: 4,
            omission_reasons: ['no baseline yet'],
          },
        },
      ],
      budget_note: 'budget note',
      growth_note: 'growth note',
      table_growth_threshold: {
        absolute_bytes: 67_108_864,
        relative_floor_bytes: 1_048_576,
        relative_percent: 10,
      },
      table_growth_coverage: {
        completeness: 'partial',
        eligible: 1,
        examined: 0,
        matched: null,
        excluded: null,
        omitted: 1,
        unknown: null,
        denominator: 1,
        unit: 'store_table_growth_reads',
        omission_reasons: ['graph.db: no baseline yet'],
      },
    });

    const store = payload.stores[0]!;
    expect(store.budget.reason).toBe('no configured budget');
    expect(store.budget.state === 'unset' && store.budget.setting_key).toBe(SETTING_KEY);
    expect(store.growth.state).toBe('unknown');
    expect(store.roles).toEqual(['graph', 'memory']);
    expect(store.read.kind).toBe('observed');
  });
});

describe('table growth presentation', () => {
  it('preserves observed omission reasons for partial coverage', () => {
    const reason = 'new_messages: no previous table watermark exists; baseline pending';
    const presentation = tableGrowthPresentation({
      state: 'observed',
      coverage: {
        completeness: 'partial',
        eligible: 1,
        examined: 0,
        matched: null,
        excluded: null,
        omitted: 1,
        unknown: null,
        denominator: 1,
        unit: 'current_tables',
        omission_reasons: [reason],
      },
      significant_samples: [],
      omissions: [
        {
          kind: 'baseline_pending',
          table: 'new_messages',
          current_bytes: 4096,
          observed_at: 20,
          reason,
        },
      ],
      omission_reasons: [reason],
    });

    expect(presentation.notes).toEqual([reason]);
    expect(presentation.summary).not.toMatch(/zero|no growth/i);
    // An observed read that could not compare every current table says so in
    // the summary, so a partial comparison never reads as a complete one.
    expect(presentation.summary).toContain('partial table coverage');
    // Partial coverage is still a measurement, not a fault.
    expect(presentation.tone).toBe('ready');
  });

  it('omits the partial-coverage wording once every current table was compared', () => {
    const presentation = tableGrowthPresentation({
      state: 'observed',
      coverage: {
        completeness: 'complete',
        eligible: 2,
        examined: 2,
        matched: null,
        excluded: null,
        omitted: 0,
        unknown: null,
        denominator: 2,
        unit: 'current_tables',
        omission_reasons: [],
      },
      significant_samples: [],
      omissions: [],
      omission_reasons: [],
    });

    expect(presentation.summary).not.toContain('partial');
    expect(presentation.notes).toEqual([]);
  });

  it('formats below-threshold omission bytes and never invents a baseline delta', () => {
    expect(
      tableGrowthOmissionPresentation({
        kind: 'below_threshold',
        table: 'metadata',
        previous_bytes: 104_857_600,
        current_bytes: 105_381_888,
        growth_bytes: 524_288,
        previous_observed_at: 10,
        current_observed_at: 20,
        reason: 'observed growth was below the informational significance threshold',
      }),
    ).toEqual({
      kind: 'below_threshold',
      table: 'metadata',
      figure: '+512.0 KiB',
      detail: '100.0 MiB → 100.5 MiB',
    });

    // A table with no previous watermark has a current size and nothing to
    // subtract it from: it must never render a delta, signed or zero.
    const pending = tableGrowthOmissionPresentation({
      kind: 'baseline_pending',
      table: 'embeddings',
      current_bytes: 4_194_304,
      observed_at: 20,
      reason: 'embeddings: no previous table watermark exists; baseline pending',
    });
    expect(pending.figure).toBe('4.0 MiB now');
    expect(pending.detail).toBe('no previous watermark · baseline pending');
    expect(pending.figure).not.toMatch(/^[+−-]/);
  });
});

describe('store budget dimension presentation', () => {
  it('reports an evaluated budget within its owner-configured soft limit', () => {
    const view = budgetPresentation({
      state: 'evaluated',
      evaluation: { state: 'within_budget', observed: 32768, soft_limit: 65536 },
      setting_key: SETTING_KEY,
      reason: 'evaluated against the owner-configured soft limit of 65536 bytes',
    });
    expect(view.state).toBe('within_budget');
    expect(view.tone).toBe('ready');
    expect(view.summary).toBe('within budget · 32.0 KiB of 64.0 KiB soft limit');
    expect(view.notes).toContain('evaluated against the owner-configured soft limit of 65536 bytes');
  });

  it('reports an evaluated over-budget store with its real overage', () => {
    const view = budgetPresentation({
      state: 'evaluated',
      evaluation: { state: 'over_budget', observed: 98304, soft_limit: 65536, overage: 32768 },
      setting_key: SETTING_KEY,
      reason: 'evaluated against the owner-configured soft limit of 65536 bytes',
    });
    expect(view.state).toBe('over_budget');
    expect(view.tone).toBe('over');
    expect(view.summary).toBe('over budget · 96.0 KiB of 64.0 KiB soft limit · over by 32.0 KiB');
  });

  it('renders an unset budget as a missing owner setting, never as unsupported or a pass', () => {
    const view = budgetPresentation({
      state: 'unset',
      reason: 'no soft size budget is configured by the owner for this store',
      setting_key: SETTING_KEY,
    });
    expect(view.state).toBe('unset');
    expect(view.summary).toBe(`no budget configured · set ${SETTING_KEY}`);
    expect(view.settingKey).toBe(SETTING_KEY);
    expect(view.summary).not.toMatch(/unsupported|within budget/);
    // An unset budget must be visually distinct from an undetermined one.
    expect(dimensionDotClass(view.tone)).not.toBe(
      dimensionDotClass(budgetPresentation({ state: 'unknown', reason: 'r' }).tone),
    );
  });

  it('renders an undetermined budget as unknown with the server reason, not a pass', () => {
    const view = budgetPresentation({
      state: 'unknown',
      reason: 'the resolved runtime configuration could not be read',
    });
    expect(view.tone).toBe('unknown');
    expect(view.summary).toBe('budget could not be determined');
    expect(view.notes).toEqual(['the resolved runtime configuration could not be read']);
    expect(view.settingKey).toBeUndefined();
  });
});

describe('store growth dimension presentation', () => {
  it('renders a shrinking store as a negative delta and an unchanged store honestly', () => {
    expect(formatSignedBytes(-2048)).toBe('−2.0 KiB');
    expect(formatSignedBytes(0)).toBe('no size change');
    expect(formatSignedBytes(1536)).toBe('+1.5 KiB');
  });

  it('renders absent execution-owned growth history as unknown', () => {
    const view = growthPresentation({
      state: 'unknown',
      reason: 'no execution-owned store-size watermark is available',
    });
    expect(view.tone).toBe('unknown');
    expect(view.summary).toBe('growth could not be determined');
    expect(view.notes[0]).toContain('execution-owned');
  });
});

describe('store role labelling', () => {
  it('names every role a shared store file serves', () => {
    expect(storeRolesLabel(['graph', 'memory'], 'graph')).toBe(
      'graph · memory (shared store file)',
    );
    expect(storeRolesLabel(['lcm'], 'lcm')).toBe('lcm');
    // A payload that somehow omits roles still names the primary role.
    expect(storeRolesLabel([], 'savings')).toBe('savings');
  });
});
