import { describe, expect, it } from 'vitest';
import type {
  CodeIndexFreshnessPayloadV1,
  DashboardDomainStateV1,
  DashboardEnvelopeV1,
  StorageFindingsPayloadV1,
  StorageTelemetryPayloadV1,
} from '../../contracts/generated.ts';
import {
  EVIDENCE_SOURCES,
  SOURCE_IDENTITY,
  coveragePercent,
  evidenceStateOf,
  findingsSummary,
  hooksSummary,
  pipelineSummary,
  relativeTickLabel,
  telemetrySummary,
  timelineModel,
  topologySummary,
  type EvidenceRead,
  type EvidenceSummary,
} from './evidence.ts';

const NOW = 1_753_003_600_000_000;

function envelope<T>(
  payload: T,
  overrides: Partial<Omit<DashboardEnvelopeV1<T>, 'payload'>> = {},
): DashboardEnvelopeV1<T> {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 3,
      examined: 3,
      matched: 3,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: 3,
      unit: 'stores',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: NOW, watermark: null },
    domain_state: 'ready',
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.storage.refresh' }],
    payload,
    ...overrides,
  };
}

function read<T>(env: DashboardEnvelopeV1<T>): EvidenceRead<T> {
  return { pending: false, result: { outcome: 'envelope', envelope: env }, updatedAtMs: 1_000 };
}

function telemetryPayload(stores: StorageTelemetryPayloadV1['stores']): StorageTelemetryPayloadV1 {
  return {
    budget_note: 'soft budgets from sync.retention.v1',
    growth_note: 'growth needs an execution-owned sampler',
    stores,
    table_growth_coverage: {
      completeness: 'unknown',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: null,
      omission_reasons: [],
    },
    table_growth_threshold: { absolute_bytes: 1, relative_floor_bytes: 1, relative_percent: 1 },
  };
}

describe('evidenceStateOf', () => {
  it('maps every wire domain state onto a typed grade and never onto measured by default', () => {
    const cases: [DashboardDomainStateV1, string][] = [
      ['ready', 'measured'],
      ['complete_zero_findings', 'empty'],
      ['partial', 'partial'],
      ['stale', 'stale'],
      ['loading', 'loading'],
      ['locked', 'restricted'],
      ['redacted', 'restricted'],
      ['unsupported', 'restricted'],
      ['unsupported_schema', 'restricted'],
      ['denied', 'denied'],
      ['unauthorized', 'denied'],
      ['error', 'failed'],
      ['cancelled', 'failed'],
      ['timed_out', 'failed'],
      ['conflicting', 'failed'],
      ['offline', 'unavailable'],
      ['unknown', 'unavailable'],
    ];
    for (const [wire, grade] of cases) expect(evidenceStateOf(wire)).toBe(grade);
    // The near neighbours from the chip taxonomy grade the same way.
    expect(evidenceStateOf('rate_limited')).toBe('partial');
    expect(evidenceStateOf('unavailable')).toBe('unavailable');
  });
});

describe('summaries', () => {
  it('names every source once with a real route', () => {
    for (const id of EVIDENCE_SOURCES) {
      expect(SOURCE_IDENTITY[id].id).toBe(id);
      expect(SOURCE_IDENTITY[id].route.startsWith('/api/')).toBe(true);
    }
  });

  it('turns an in-flight read into loading and a missing one into unavailable, with nothing measured', () => {
    const pending = telemetrySummary({ pending: true, result: undefined, updatedAtMs: 0 });
    expect(pending.state).toBe('loading');
    expect(pending.coverage).toBeNull();
    expect(pending.observedAtMicros).toBeNull();
    const missing = telemetrySummary({ pending: false, result: undefined, updatedAtMs: 0 });
    expect(missing.state).toBe('unavailable');
    expect(missing.stateDetail).toBe('no response recorded');
  });

  it('keeps a transport failure as the daemon state it was reported as', () => {
    const offline = telemetrySummary({
      pending: false,
      result: { outcome: 'transport', state: 'offline' },
      updatedAtMs: 0,
    });
    expect(offline.state).toBe('unavailable');
    expect(offline.stateDetail).toBe('offline');
    const denied = telemetrySummary({
      pending: false,
      result: { outcome: 'transport', state: 'denied', detail: 'identity lacks storage read' },
      updatedAtMs: 0,
    });
    expect(denied.state).toBe('denied');
    expect(denied.stateDetail).toBe('identity lacks storage read');
    expect(denied.refreshOperation).toBeNull();
  });

  it('lifts the envelope truth header and grades a served-empty telemetry read as measured-empty', () => {
    const summary = telemetrySummary(read(envelope(telemetryPayload([]))));
    expect(summary.state).toBe('empty');
    expect(summary.stateDetail).toBe('telemetry payload contained no stores');
    expect(summary.coverage).toEqual({
      completeness: 'complete',
      examined: 3,
      denominator: 3,
      unit: 'stores',
    });
    expect(summary.observedAtMicros).toBe(NOW);
    expect(summary.scope?.projectId).toBe('tracedecay');
    expect(summary.authorization).toBe('authorized');
    expect(summary.refreshOperation).toBe('use-case.dashboard.storage.refresh');
    expect(summary.declaredActions).toEqual([
      { kind: 'refresh', operation: 'use-case.dashboard.storage.refresh' },
    ]);
    expect(summary.lastReadMs).toBe(1_000);
  });

  it('never promotes a partial envelope to measured, whatever the payload says', () => {
    const summary = telemetrySummary(
      read(envelope(telemetryPayload([]), { domain_state: 'partial' })),
    );
    expect(summary.state).toBe('partial');
  });

  it('grades an in-flight code-index build as building and an unmounted scope as empty', () => {
    const worktree: CodeIndexFreshnessPayloadV1['worktrees'][number] = {
      worktree_root: '/w',
      repository_id: null,
      worktree_id: null,
      source_reference: null,
      source_revision: null,
      latest_generation_id: null,
      snapshot_content_identity: null,
      sealed_at_micros: null,
      last_reconcile_micros: null,
      staleness_state: 'indexing',
      rebuild_in_flight: false,
      hook_hint_count: 0,
      coverage: 'partial',
      parked: null,
      progress: {
        generation_id: 'g1',
        daemon_incarnation: 1,
        producer_incarnation: 1,
        progress_epoch: 1,
        sealed_source_digest: 'sha256:x',
        phase: 'bulk_commit',
        committed_pages: 1,
        committed_chunks: 1,
        committed_imports: 0,
        committed_payload_bytes: 1,
        completed_files: 1,
        total_files: 2,
        completed_lexical_units: 1,
        total_lexical_units: 2,
        current_batch_pages: 1,
        current_batch_payload_bytes: 1,
        elapsed_micros: 1,
        last_commit_latency_micros: null,
        files_per_second: null,
        lexical_units_per_second: null,
        estimated_remaining_seconds: null,
        last_progress_micros: NOW,
        blocked_reason: 'retry_backoff',
      },
    };
    const building = pipelineSummary(
      read(envelope({ note: 'live', worktrees: [worktree] }, { domain_state: 'loading' })),
    );
    expect(building.state).toBe('building');
    expect(building.stateDetail).toBe('1 of 1 worktrees building · 1 blocked');
    expect(building.affected).toBe('1 worktrees · 1 building · 0 stale · 1 blocked');

    const empty = pipelineSummary(read(envelope({ note: 'live', worktrees: [] })));
    expect(empty.state).toBe('empty');
    expect(empty.stateDetail).toBe('no mounted code-index worktree');

    // A refused envelope stays refused even when the payload names a build.
    const denied = pipelineSummary(
      read(envelope({ note: 'live', worktrees: [worktree] }, { domain_state: 'denied' })),
    );
    expect(denied.state).toBe('denied');
  });

  it('grades an unavailable hint source as unavailable with the daemon reason, not as an empty table', () => {
    const summary = hooksSummary(
      read(
        envelope({
          available: false,
          by_category: [],
          error: 'hint store unreadable',
          source: 'hook_analytics.jsonl',
        }),
      ),
    );
    expect(summary.state).toBe('unavailable');
    expect(summary.stateDetail).toBe('hint store unreadable');
    expect(summary.affected).toBeNull();
  });

  it('counts real producers as findings coverage and grades a served zero-findings report as empty', () => {
    const payload: StorageFindingsPayloadV1 = {
      entries: [],
      family_filter: 'storage',
      kind_statuses: [
        { kind: 'over_budget_store', state: 'real', reason: 'measured', observed_entries: 0 },
        { kind: 'orphan_store', state: 'partial', reason: 'one store unreadable', observed_entries: 0 },
      ],
      known_families: ['storage'],
      note: 'no entries',
      report_coverage: null,
      schema_convergences: [],
    };
    const summary = findingsSummary(read(envelope(payload)));
    expect(summary.state).toBe('empty');
    expect(summary.coverage).toEqual({
      completeness: 'complete',
      examined: 1,
      denominator: 2,
      unit: 'producers real',
    });
    expect(coveragePercent(summary.coverage)).toBe(50);
  });

  it('grades the topology projection by its own family coverage and refusal', () => {
    const refused = topologySummary({
      pending: false,
      result: { outcome: 'refused', state: 'locked', detail: 'read-only project' },
      updatedAtMs: 0,
    });
    expect(refused.state).toBe('restricted');
    expect(refused.stateDetail).toBe('read-only project');
  });
});

describe('coveragePercent', () => {
  it('refuses to print a percent without both figures', () => {
    expect(coveragePercent(null)).toBeNull();
    expect(
      coveragePercent({ completeness: 'partial', examined: 4, denominator: null, unit: null }),
    ).toBeNull();
    expect(
      coveragePercent({ completeness: 'partial', examined: null, denominator: 4, unit: null }),
    ).toBeNull();
    expect(
      coveragePercent({ completeness: 'unknown', examined: 0, denominator: 0, unit: null }),
    ).toBeNull();
    expect(
      coveragePercent({ completeness: 'partial', examined: 1, denominator: 4, unit: null }),
    ).toBe(25);
  });
});

describe('timelineModel', () => {
  function summary(id: EvidenceSummary['id'], observedAtMicros: number | null): EvidenceSummary {
    return {
      ...SOURCE_IDENTITY[id],
      state: 'measured',
      stateDetail: null,
      coverage: null,
      observedAtMicros,
      freshness: null,
      scope: null,
      authorization: null,
      watermark: null,
      affected: null,
      note: null,
      declaredActions: [],
      refreshOperation: null,
      lastReadMs: 0,
    };
  }

  it('places marks between the oldest and newest read and lists unplaced sources as absences', () => {
    const model = timelineModel([
      summary('telemetry', NOW - 3_600_000_000),
      summary('findings', NOW),
      summary('doctor', null),
    ]);
    expect(model.extent).toEqual({ oldestMicros: NOW - 3_600_000_000, newestMicros: NOW });
    expect(model.marks.map((mark) => [mark.id, mark.position])).toEqual([
      ['telemetry', 0],
      ['findings', 1],
    ]);
    expect(model.unplaced).toEqual([{ id: 'doctor', title: 'Doctor inspection', state: 'measured' }]);
  });

  it('folds marks closer than one hit target into a cluster that keeps every member', () => {
    const model = timelineModel(
      [
        summary('telemetry', NOW),
        summary('findings', NOW - 1_000),
        summary('doctor', NOW - 3_600_000_000),
      ],
      0.04,
    );
    expect(model.marks).toHaveLength(3);
    expect(model.clusters.map((cluster) => cluster.marks.map((mark) => mark.id))).toEqual([
      ['doctor'],
      ['findings', 'telemetry'],
    ]);
    const crowded = model.clusters[1]!;
    expect(crowded.position).toBe(1);
    expect(crowded.oldestMicros).toBe(NOW - 1_000);
    expect(crowded.newestMicros).toBe(NOW);
  });

  it('places every mark at the rail end when all reads share one instant', () => {
    const model = timelineModel([summary('telemetry', NOW), summary('findings', NOW)]);
    expect(model.marks.every((mark) => mark.position === 1)).toBe(true);
    expect(model.clusters).toHaveLength(1);
    expect(model.clusters[0]?.marks).toHaveLength(2);
  });

  it('has no extent when nothing published a time', () => {
    const model = timelineModel([summary('telemetry', null)]);
    expect(model.extent).toBeNull();
    expect(model.marks).toEqual([]);
    expect(model.unplaced).toHaveLength(1);
  });
});

describe('relativeTickLabel', () => {
  it('measures back from the newest read in the coarsest honest unit', () => {
    expect(relativeTickLabel(0)).toBe('newest');
    expect(relativeTickLabel(45_000_000)).toBe('-45s');
    expect(relativeTickLabel(600_000_000)).toBe('-10m');
    expect(relativeTickLabel(7_200_000_000)).toBe('-2h');
    expect(relativeTickLabel(3 * 86_400_000_000)).toBe('-3d');
  });
});
