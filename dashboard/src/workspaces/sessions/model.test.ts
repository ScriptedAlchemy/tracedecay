import { describe, expect, it } from 'vitest';
import type { LoomSessionRowV1, LoomTemporalPayloadV1 } from '../../contracts/generated.ts';
import {
  bucketKeyFor,
  bucketStart,
  commitEvidence,
  joinIndexRow,
  pageBounds,
  provenanceTally,
  readViewState,
  recordedModels,
  relationsFor,
  sessionExtent,
  writeViewState,
} from './model.ts';

function row(over: Partial<LoomSessionRowV1> = {}): LoomSessionRowV1 {
  return {
    session_id: 'sess-1',
    provider: 'claude',
    title: null,
    started_at: 1_700_000_000,
    ended_at: null,
    last_message_at: null,
    messages: 3,
    models: [],
    is_subagent: false,
    edited_files_recorded: false,
    ...over,
  };
}

describe('view state round trip', () => {
  it('reads defaults from an empty URL and writes defaults as absence', () => {
    const defaults = readViewState(new URLSearchParams());
    expect(defaults).toEqual({ selection: null, page: 1, rows: 25, bucket: 'day', window: 400 });
    const written = writeViewState(new URLSearchParams('scope=p1&scopeLabel=P1'), defaults);
    expect(written.toString()).toBe('scope=p1&scopeLabel=P1');
  });

  it('round-trips a selection, page, rows, bucket and window', () => {
    const state = {
      selection: { provider: 'codex', sessionId: 'a:b/c' },
      page: 7,
      rows: 100,
      bucket: 'hour',
      window: 2000,
    } as const;
    const params = writeViewState(new URLSearchParams(), state);
    expect(readViewState(params)).toEqual(state);
  });

  it('keeps a provider-less selection provider-less', () => {
    const state = readViewState(new URLSearchParams('sessionId=only-id'));
    expect(state.selection).toEqual({ provider: null, sessionId: 'only-id' });
  });

  it('falls back to defaults for values the routes do not accept', () => {
    const state = readViewState(
      new URLSearchParams('sessionsPage=0&sessionsRows=37&sessionsBucket=week&sessionsWindow=12'),
    );
    expect(state.page).toBe(1);
    expect(state.rows).toBe(25);
    expect(state.bucket).toBe('day');
    expect(state.window).toBe(400);
  });
});

describe('index paging', () => {
  it('states loaded bounds and page count against the store total', () => {
    expect(pageBounds(3, 25, 25, 2_847)).toEqual({ first: 51, last: 75, pageCount: 114 });
  });

  it('never reports bounds for an empty page and never a count without a total', () => {
    expect(pageBounds(1, 25, 0, 0)).toEqual({ first: null, last: null, pageCount: 1 });
    expect(pageBounds(2, 50, 12, null)).toEqual({ first: 51, last: 62, pageCount: null });
  });
});

describe('session extent', () => {
  it('types each recorded shape without consulting the clock', () => {
    expect(sessionExtent(row({ started_at: null }))).toEqual({ kind: 'undated' });
    expect(sessionExtent(row({ ended_at: 1_700_000_900 }))).toEqual({
      kind: 'ended',
      start: 1_700_000_000,
      end: 1_700_000_900,
    });
    expect(sessionExtent(row({ last_message_at: 1_700_000_400 }))).toEqual({
      kind: 'open',
      start: 1_700_000_000,
      last: 1_700_000_400,
    });
    expect(sessionExtent(row())).toEqual({ kind: 'open_unobserved', start: 1_700_000_000 });
  });

  it('counts unrecorded models separately from recorded identities', () => {
    expect(
      recordedModels(
        row({ models: [{ model: null }, { model: 'gpt-5.6-sol-high' }, { model: 'gpt-5.6-sol-high' }] }),
      ),
    ).toEqual({ models: ['gpt-5.6-sol-high'], unrecorded: 1 });
  });
});

describe('selection join', () => {
  const rows = [
    row({ provider: 'claude', session_id: 'shared' }),
    row({ provider: 'codex', session_id: 'shared' }),
    row({ provider: 'cursor', session_id: 'solo' }),
  ];

  it('joins exactly when the provider is known', () => {
    expect(joinIndexRow(rows, { provider: 'codex', sessionId: 'shared' })).toEqual({
      kind: 'exact',
      row: rows[1],
    });
  });

  it('keeps two providers ambiguous instead of picking one', () => {
    const join = joinIndexRow(rows, { provider: null, sessionId: 'shared' });
    expect(join.kind).toBe('ambiguous');
    if (join.kind === 'ambiguous') expect(join.rows).toHaveLength(2);
  });

  it('reports a session outside the loaded page as absent from the page', () => {
    expect(joinIndexRow(rows, { provider: 'claude', sessionId: 'elsewhere' })).toEqual({
      kind: 'absent',
    });
  });
});

describe('relations', () => {
  it('filters every relation by provider and session and names its source status', () => {
    const payload: LoomTemporalPayloadV1 = {
      available: true,
      total: 2,
      sessions: [],
      commits: [
        {
          session_id: 's',
          provider: 'claude',
          commit_sha: 'a'.repeat(40),
          committed_at: 1,
          branch: null,
          worktree: null,
          relation: 'produced',
          evidence: 'tool_result',
          span_overlap_kind: null,
          confidence: 1,
        },
        {
          session_id: 's',
          provider: 'codex',
          commit_sha: 'b'.repeat(40),
          committed_at: 1,
          branch: null,
          worktree: null,
          relation: 'observed',
          evidence: 'time_overlap',
          span_overlap_kind: 'extended_window',
          confidence: 0.4,
        },
      ],
      edited_files: [{ session_id: 's', provider: 'claude', path: 'a.ts', change_type: null, hunks: null }],
      branch_spans: [],
      source_statuses: [
        {
          id: 'session_commit',
          label: 'Session → commit',
          state: 'ready',
          authority: 'git',
          granularity: 'commit',
          providers: [],
          item_count: 2,
          reason: null,
          required_authority: null,
          coverage: {
            completeness: 'complete',
            eligible: 2,
            examined: 2,
            matched: 2,
            omitted: 0,
            unit: 'commits',
            reason: 'all',
          },
        },
      ],
      temporal_refresh: {
        state: 'ready',
        active_generations: 1,
        latest_activated_at_micros: null,
        authority: 'x',
      },
    };
    const relations = relationsFor(payload, 'claude', 's');
    expect(relations.commits.map((c) => c.commit_sha)).toEqual(['a'.repeat(40)]);
    expect(relations.editedFiles).toHaveLength(1);
    expect(relations.commitStatus?.id).toBe('session_commit');
    expect(relations.fileStatus).toBeNull();
  });
});

describe('commit evidence on the ladder', () => {
  it('grades direct records EXACT and correlations INFERRED, never by confidence', () => {
    expect(commitEvidence({ evidence: 'tool_result' })).toEqual({
      grade: 'EXACT',
      sourceClass: 'TOOL RESULT',
    });
    expect(commitEvidence({ evidence: 'head_observation' })?.grade).toBe('EXACT');
    expect(commitEvidence({ evidence: 'reflog_overlap' })?.grade).toBe('INFERRED');
    expect(commitEvidence({ evidence: 'time_overlap' })?.grade).toBe('INFERRED');
  });

  it('leaves an unknown evidence class ungraded', () => {
    expect(commitEvidence({ evidence: 'vibes' })).toBeNull();
  });
});

describe('timeline buckets', () => {
  it('reproduces the server UTC bucket keys and parses them back', () => {
    // 2026-08-05T13:07:00Z
    const instant = Date.UTC(2026, 7, 5, 13, 7) / 1000;
    expect(bucketKeyFor(instant, 'day')).toBe('2026-08-05');
    expect(bucketKeyFor(instant, 'hour')).toBe('2026-08-05T13:00');
    expect(bucketStart('2026-08-05')).toBe(Date.UTC(2026, 7, 5) / 1000);
    expect(bucketStart('2026-08-05T13:00')).toBe(Date.UTC(2026, 7, 5, 13) / 1000);
    expect(bucketStart('yesterday')).toBeNull();
  });

  it('tallies token provenance across dated buckets and the undated gutter', () => {
    const tally = provenanceTally(
      [
        {
          bucket: '2026-08-05',
          count: 10,
          known_message_count: 8,
          unknown_message_count: 2,
          token_count: 800,
          token_count_provenance: 'o200k_approximate',
        },
        {
          bucket: '2026-08-06',
          count: 5,
          known_message_count: 0,
          unknown_message_count: 5,
          token_count: null,
          token_count_provenance: 'unavailable',
        },
      ],
      {
        count: 3,
        known_message_count: 1,
        unknown_message_count: 2,
        token_count: null,
        token_count_provenance: 'unavailable',
      },
    );
    expect(tally).toEqual({ known: 9, unknown: 9, dated: 15, undated: 3 });
  });
});
