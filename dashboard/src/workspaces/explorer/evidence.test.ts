import { describe, expect, it } from 'vitest';
import { hitEvidence, laneGrade } from './evidence.ts';
import { semanticLane, type ExplorerLaneReadModel } from './laneModel.ts';
import { codeHits, knowledgeHits, sessionHits } from './model.ts';

describe('hitEvidence', () => {
  it('grades every identity EXACT and quotes transcript and fact text as EXPLICIT claims', () => {
    const [code] = codeHits([{ id: 'n1', name: 'graph_search', signature: 'fn graph_search()' }], []);
    const [message] = sessionHits(
      [{ message_id: 'm1', session_id: 's1', snippet: 'let us use graph search' }],
      [],
    );
    const [fact] = knowledgeHits([{ fact_id: 'f1', content: 'graph search is bounded' }], []);

    expect(hitEvidence(code!)).toMatchObject({ sourceClass: 'GRAPH', identity: 'EXACT', text: 'EXACT' });
    expect(hitEvidence(message!)).toMatchObject({
      sourceClass: 'TRANSCRIPT',
      identity: 'EXACT',
      text: 'EXPLICIT',
    });
    expect(hitEvidence(fact!)).toMatchObject({ sourceClass: 'FACT', identity: 'EXACT', text: 'EXPLICIT' });
    // The basis names the field the quoted text came from, so the grade is checkable.
    expect(hitEvidence(message!).basis).toContain('snippet');
  });
});

describe('laneGrade', () => {
  it('serves rows EXACT, a stale store STALE, an unanswered lane UNAVAILABLE, and a reading lane nothing', () => {
    const ready: ExplorerLaneReadModel = {
      state: 'ready',
      lane: 'code',
      hits: [],
      reportedTotal: 0,
      unreadableRows: 0,
      hasMore: false,
    };
    const stale: ExplorerLaneReadModel = { state: 'stale', lane: 'code', errorCode: null, detail: null };
    const pending: ExplorerLaneReadModel = { state: 'pending', lane: 'code', phase: 'reading' };
    const locked: ExplorerLaneReadModel = { state: 'locked', lane: 'code', detail: 'read-only' };

    expect(laneGrade(ready)).toBe('EXACT');
    expect(laneGrade(stale)).toBe('STALE');
    expect(laneGrade(pending)).toBeNull();
    expect(laneGrade(locked)).toBe('UNAVAILABLE');
    expect(laneGrade({ state: 'offline', lane: 'sessions' })).toBe('UNAVAILABLE');
    expect(laneGrade(semanticLane())).toBe('UNAVAILABLE');
  });
});
