import { describe, expect, it } from 'vitest';
import {
  DoctorFindingsPayloadSchema,
  EnvelopeSchema,
  LegalActionRefSchema,
  StorageTelemetryPayloadSchema,
} from '../../contracts/wire.ts';
import { doctorEvidencePresentation } from './doctorModel.ts';
import {
  budgetPresentation,
  dimensionDotClass,
  formatSignedBytes,
  growthPresentation,
  refreshOperation,
  storageFindingLabel,
  storeRolesLabel,
} from './storageModel.ts';

const SETTING_KEY = 'sync.retention.v1 store_soft_budgets_bytes';
const COVERAGE =
  'since-daemon-start: bounded in-process watermark ring recorded on each telemetry sample, not a persisted historical series';

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
      LegalActionRefSchema.safeParse({
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

    expect(EnvelopeSchema(StorageTelemetryPayloadSchema).safeParse(envelope).success).toBe(false);
  });

  it('decodes the canonical Doctor storage-family projection and maps typed kinds', () => {
    const payload = DoctorFindingsPayloadSchema.parse({
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
        ] as const
      ).map(storageFindingLabel),
    ).toEqual([
      'Over-budget stores',
      'Orphan stores',
      'Stale branch databases',
      'Incident debris',
      'Retention backlog',
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
    const payload = StorageTelemetryPayloadSchema.parse({
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
            state: 'baseline',
            coverage: COVERAGE,
            measured_at: 123,
            total_bytes: 32768,
            reason: 'first watermark recorded in this daemon lifetime',
          },
        },
      ],
      budget_note: 'budget note',
      growth_note: 'growth note',
    });

    const store = payload.stores[0]!;
    expect(store.budget.reason).toBe('no configured budget');
    expect(store.budget.state === 'unset' && store.budget.setting_key).toBe(SETTING_KEY);
    expect(store.growth.state).toBe('baseline');
    expect(store.roles).toEqual(['graph', 'memory']);
    expect(store.read.kind).toBe('observed');
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
  it('states a baseline as a real first sample, not zero growth', () => {
    const view = growthPresentation({
      state: 'baseline',
      coverage: COVERAGE,
      measured_at: 1,
      total_bytes: 32768,
      reason: 'first watermark recorded in this daemon lifetime',
    });
    expect(view.state).toBe('baseline');
    expect(view.summary).toContain('first sample this daemon lifetime — not zero growth');
    expect(view.summary).toContain('32.0 KiB measured');
    // The coverage sentence is surfaced verbatim, never paraphrased.
    expect(view.notes).toContain(COVERAGE);
  });

  it('shows a signed delta and the verbatim since-daemon-start coverage', () => {
    const grew = growthPresentation({
      state: 'observed',
      coverage: COVERAGE,
      first_measured_at: 1,
      last_measured_at: 2,
      sample_count: 12,
      first_total_bytes: 32768,
      current_total_bytes: 65536,
      growth_bytes: 32768,
      samples: [
        { measured_at: 1, total_bytes: 32768, free_bytes: 0 },
        { measured_at: 2, total_bytes: 65536, free_bytes: 0 },
      ],
    });
    expect(grew.summary).toBe(
      '+32.0 KiB over 12 store-size watermarks · 32.0 KiB → 64.0 KiB',
    );
    expect(grew.notes).toEqual([COVERAGE]);
    // The retired per-table wording must be gone: these are store watermarks.
    expect(grew.summary).not.toMatch(/table samples/);
  });

  it('renders a shrinking store as a negative delta and an unchanged store honestly', () => {
    expect(formatSignedBytes(-2048)).toBe('−2.0 KiB');
    expect(formatSignedBytes(0)).toBe('no size change');
    expect(formatSignedBytes(1536)).toBe('+1.5 KiB');
  });

  it('renders an unrecordable growth read as unknown', () => {
    const view = growthPresentation({
      state: 'unknown',
      reason: 'no watermark could be recorded because the store size read did not produce a sample',
    });
    expect(view.tone).toBe('unknown');
    expect(view.summary).toBe('growth could not be determined');
    expect(view.notes[0]).toContain('no watermark could be recorded');
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
