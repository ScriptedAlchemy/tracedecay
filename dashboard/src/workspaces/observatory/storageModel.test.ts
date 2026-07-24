import { describe, expect, it } from 'vitest';
import {
  EnvelopeSchema,
  LegalActionRefSchema,
  StorageFindingsPayloadSchema,
  StorageTelemetryPayloadSchema,
} from '../../contracts/wire.ts';
import {
  doctorEvidencePresentation,
  refreshOperation,
  storageFindingLabel,
} from './storageModel.ts';

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

  it('decodes doctor report entries and maps storage kinds to labels', () => {
    const entry = (
      storageKind: string,
      state: string,
      statement: string,
    ) => ({
      finding: {
        family: 'storage',
        state,
        evidence: [{ family: 'storage', reference: `evidence.${storageKind}` }],
        coverage: { completeness: 'partial', statement },
        remediation: null,
      },
      storage_kind: storageKind,
    });
    const payload = StorageFindingsPayloadSchema.parse({
      family_filter: 'storage',
      entries: [
        entry('over_budget_store', 'unsupported', 'budget source unavailable'),
        entry('orphan_store', 'absent', 'no orphan stores observed'),
        entry('stale_branch_dbs', 'stale', 'inventory watermark is stale'),
        entry('incident_debris_present', 'degraded', 'quarantined debris is present'),
        entry('retention_backlog', 'partial', 'backlog scan was partial'),
      ],
      report_coverage: null,
      remediations: [],
      known_families: ['storage'],
      note: 'storage evidence',
    });

    expect(
      payload.entries.map((row) =>
        row.storage_kind ? storageFindingLabel(row.storage_kind) : row.finding.family,
      ),
    ).toEqual([
      'Over-budget stores',
      'Orphan stores',
      'Stale branch databases',
      'Incident debris',
      'Retention backlog',
    ]);
    expect(payload.entries[0]?.finding.coverage.statement).toBe('budget source unavailable');
    expect(doctorEvidencePresentation(payload.entries[3]!.finding.state)).toEqual({
      label: 'Degraded',
      tokenClass: 'text-state-error',
      dotClass: 'bg-state-error',
    });
  });

  it('preserves typed absent dimensions and observed samples', () => {
    const payload = StorageTelemetryPayloadSchema.parse({
      stores: [
        {
          store: 'graph.db',
          role: 'graph',
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
          budget: {
            state: 'unsupported',
            reason: 'no configured budget',
          },
          growth: {
            state: 'absent',
            reason: 'no growth watermark',
          },
        },
      ],
      budget_note: 'budget note',
      growth_note: 'growth note',
    });

    expect(payload.stores[0]?.budget.reason).toBe('no configured budget');
    const growth = payload.stores[0]?.growth;
    expect(growth?.state).toBe('absent');
    if (growth?.state === 'absent') {
      expect(growth.reason).toBe('no growth watermark');
    }
    expect(payload.stores[0]?.read.kind).toBe('observed');
  });
});
