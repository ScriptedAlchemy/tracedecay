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

  it('decodes storage finding kind statuses and maps storage kinds to labels', () => {
    // Mirrors StorageFindingsPayloadV1 in src/dashboard/storage_findings_api.rs:
    // a per-kind producer support report, not a Doctor report projection.
    const kind = (storageKind: string, state: string, reason: string) => ({
      kind: storageKind,
      state,
      required_source: `${storageKind}_source`,
      reason,
    });
    const payload = StorageFindingsPayloadSchema.parse({
      kinds: [
        kind('over_budget_store', 'unsupported', 'budget source unavailable'),
        kind('orphan_store', 'absent', 'no orphan stores observed'),
        kind('stale_branch_dbs', 'stale', 'inventory watermark is stale'),
        kind('incident_debris_present', 'degraded', 'quarantined debris is present'),
        kind('retention_backlog', 'partial', 'backlog scan was partial'),
      ],
      note: 'storage evidence',
    });

    expect(payload.kinds.map((row) => storageFindingLabel(row.kind))).toEqual([
      'Over-budget stores',
      'Orphan stores',
      'Stale branch databases',
      'Incident debris',
      'Retention backlog',
    ]);
    expect(payload.kinds[0]?.reason).toBe('budget source unavailable');
    expect(doctorEvidencePresentation(payload.kinds[3]!.state)).toEqual({
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
