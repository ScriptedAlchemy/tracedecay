import { describe, expect, it } from 'vitest';
import { WorkGraphReadV1Schema, type WorkGraphReadV1 } from '../../contracts/index.ts';
import { workGraphRead, workGraphTimeline } from '../../test/workGraphFixture.ts';
import type { WorkResult } from './workApi.ts';
import {
  WORK_EVIDENCE_PAGE_SIZE,
  workEvidenceAuthorityKey,
  workEvidenceRequest,
  workEvidenceTemporalMode,
} from './workEvidenceQueries.ts';

function current(): WorkResult<WorkGraphReadV1> {
  return {
    outcome: 'value',
    value: WorkGraphReadV1Schema.parse(workGraphRead({ tasks: [{ taskId: 'task.alpha' }] })),
  };
}

describe('TaskSession evidence requests', () => {
  it('binds the task and selected temporal mode to the exact current graph authority', () => {
    const graph = current();
    const request = workEvidenceRequest(graph, 'task.alpha', { kind: 'evolution' }, null, 123);
    expect(request).toEqual({
      selection: { selection: 'profile_owned_no_git' },
      task_id: 'task.alpha',
      verified_version:
        graph.outcome === 'value' && graph.value.mode === 'current'
          ? graph.value.snapshot.verified_version
          : undefined,
      temporal: { kind: 'evolution' },
      page_size: WORK_EVIDENCE_PAGE_SIZE,
      expansion: null,
      continuation: null,
      observed_at: 123,
    });
  });

  it('changes the cache authority when the exact graph version changes', () => {
    const first = current();
    const second: WorkResult<WorkGraphReadV1> = {
      outcome: 'value',
      value: WorkGraphReadV1Schema.parse(
        workGraphRead({
          version: 2,
          tasks: [{ taskId: 'task.alpha' }],
        }),
      ),
    };

    expect(workEvidenceAuthorityKey(first, 'task.alpha', { kind: 'current' })).not.toBe(
      workEvidenceAuthorityKey(second, 'task.alpha', { kind: 'current' }),
    );
  });

  it('separates cache authority for every temporal mode and exact as-of cutoff', () => {
    const graph = current();
    const keys = [
      workEvidenceAuthorityKey(graph, 'task.alpha', { kind: 'current' }),
      workEvidenceAuthorityKey(graph, 'task.alpha', { kind: 'as_of', cutoff: 100 }),
      workEvidenceAuthorityKey(graph, 'task.alpha', { kind: 'as_of', cutoff: 101 }),
      workEvidenceAuthorityKey(graph, 'task.alpha', { kind: 'evolution' }),
      workEvidenceAuthorityKey(graph, 'task.alpha', { kind: 'forensic' }),
    ];
    expect(new Set(keys).size).toBe(keys.length);
  });

  it('converts an explicit UTC cutoff to canonical microseconds without a default', () => {
    expect(workEvidenceTemporalMode('as_of', '')).toBeUndefined();
    expect(workEvidenceTemporalMode('as_of', 'not-a-date')).toBeUndefined();
    expect(workEvidenceTemporalMode('as_of', '2026-08-10T12:34:56.789')).toEqual({
      kind: 'as_of',
      cutoff: 1_786_365_296_789_000,
    });
    expect(workEvidenceTemporalMode('current', '')).toEqual({ kind: 'current' });
    expect(workEvidenceTemporalMode('evolution', '')).toEqual({ kind: 'evolution' });
    expect(workEvidenceTemporalMode('forensic', '')).toEqual({ kind: 'forensic' });
  });

  it('pairs a TaskSession continuation with its exact accepted attempt', () => {
    const continuation = {
      kind: 'task_session' as const,
      continuation: {
        attempt: { task_id: 'task.alpha', run_id: 'run.1', attempt_id: 'attempt.1' },
        participant_epoch: 'digest.participants',
        ranking_cursor: 'ranking.cursor',
        source: { provider: 'codex', session_id: 'session.1' },
        temporal_cursor: 'temporal.cursor',
        verified_version: {
          event_sequence: 12,
          graph_version: 1,
          recovered_graph_digest: 'digest-graph',
          source_watermark: {},
        },
      },
    };
    const request = workEvidenceRequest(
      current(),
      'task.alpha',
      { kind: 'forensic' },
      continuation,
      123,
    );
    expect(request?.continuation).toEqual(continuation);
    expect(request?.expansion).toEqual({
      kind: 'task_session',
      attempt: continuation.continuation.attempt,
    });
  });

  it('does not invent a snapshot identity from a timeline or refusal', () => {
    const timeline: WorkResult<WorkGraphReadV1> = {
      outcome: 'value',
      value: WorkGraphReadV1Schema.parse(
        workGraphTimeline([{ tasks: [{ taskId: 'task.alpha' }] }]),
      ),
    };
    expect(
      workEvidenceRequest(timeline, 'task.alpha', { kind: 'current' }, null, 123),
    ).toBeUndefined();
    expect(
      workEvidenceRequest(
        { outcome: 'refused', state: 'unavailable', detail: 'not mounted' },
        'task.alpha',
        { kind: 'current' },
        null,
        123,
      ),
    ).toBeUndefined();
  });
});
