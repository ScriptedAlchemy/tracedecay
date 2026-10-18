import { describe, expect, it } from 'vitest';
import type { LoomSessionRowV1 } from '../../contracts/generated.ts';
import {
  parseHiddenKinds,
  parseLaneSet,
  parseLoomSession,
  parseWindow,
  parseZoom,
  resolveLoomSession,
  serializeHiddenKinds,
  serializeLaneSet,
  serializeWindow,
  toggleInSet,
} from './loomUrl.ts';

function row(provider: string, sessionId: string): LoomSessionRowV1 {
  return {
    session_id: sessionId,
    provider,
    title: null,
    started_at: 1_784_700_000,
    ended_at: null,
    last_message_at: null,
    messages: 1,
    models: [],
    is_subagent: false,
    edited_files_recorded: true,
  };
}

describe('loomUrl', () => {
  it('round-trips a window and refuses one that is not a time this store could record', () => {
    const window = { start: 1_784_700_000, end: 1_784_703_600 };
    expect(parseWindow(serializeWindow(window))).toEqual(window);
    // The legacy replay parameter carried 0..1 fractions; they must not become a window.
    expect(parseWindow('0.25,0.75')).toBeNull();
    expect(parseWindow('1784700000,1784700000')).toBeNull();
    expect(parseWindow('1784703600,1784700000')).toBeNull();
    expect(parseWindow('nonsense')).toBeNull();
    expect(parseWindow(null)).toBeNull();
  });

  it('carries lane ids as one JSON array because the ids are JSON tuples themselves', () => {
    const ids = new Set(['["cursor","a:b,c"]', '["codex","d"]']);
    const raw = serializeLaneSet(ids);
    expect(raw).not.toBeNull();
    expect(parseLaneSet(raw)).toEqual(ids);
    expect(serializeLaneSet(new Set())).toBeNull();
    expect(parseLaneSet('not json')).toEqual(new Set());
    expect(parseLaneSet('{"a":1}')).toEqual(new Set());
    expect(parseLaneSet('["x", 1, null]')).toEqual(new Set(['x']));
  });

  it('keeps only known event kinds in the hidden set, in canonical order', () => {
    expect(parseHiddenKinds('commit,spawn,bogus')).toEqual(new Set(['commit', 'spawn']));
    expect(serializeHiddenKinds(new Set(['spawn', 'commit']))).toBe('spawn,commit');
    expect(serializeHiddenKinds(new Set())).toBeNull();
  });

  it('only admits the two reader-selectable zoom levels', () => {
    expect(parseZoom('workstream')).toBe('workstream');
    expect(parseZoom('event')).toBe('agent');
    expect(parseZoom(null)).toBe('agent');
  });

  it('reads loomSession as a lane tuple or as the bare id Delivery links with', () => {
    expect(parseLoomSession(null)).toBeNull();
    expect(parseLoomSession('')).toBeNull();
    expect(parseLoomSession('["claude","a"]')).toEqual({
      kind: 'lane',
      laneId: '["claude","a"]',
      selection: { provider: 'claude', sessionId: 'a' },
    });
    expect(parseLoomSession('["unknown","a"]')).toEqual({
      kind: 'lane',
      laneId: '["unknown","a"]',
      selection: { provider: null, sessionId: 'a' },
    });
    expect(parseLoomSession('session.alpha.1')).toEqual({
      kind: 'bare_id',
      selection: { provider: null, sessionId: 'session.alpha.1' },
    });
    expect(parseLoomSession('["claude"')).toEqual({
      kind: 'bare_id',
      selection: { provider: null, sessionId: '["claude"' },
    });
    expect(parseLoomSession('["claude"]')).toMatchObject({ kind: 'bare_id' });
  });

  it('resolves a link against the loaded page without guessing between providers', () => {
    const claude = row('claude', 'shared');
    const codex = row('codex', 'shared');
    const unrecorded = row('', 'quiet');
    const rows = [claude, codex, unrecorded];
    const resolve = (raw: string | null) => resolveLoomSession(rows, parseLoomSession(raw));

    expect(resolve(null)).toEqual({ kind: 'none' });
    expect(resolve('["codex","shared"]')).toEqual({ kind: 'lane', laneId: '["codex","shared"]', row: codex });
    expect(resolve('["unknown","quiet"]')).toEqual({ kind: 'lane', laneId: '["unknown","quiet"]', row: unrecorded });
    expect(resolve('quiet')).toEqual({ kind: 'lane', laneId: '["unknown","quiet"]', row: unrecorded });
    expect(resolve('["","quiet"]')).toEqual({ kind: 'lane', laneId: '["unknown","quiet"]', row: unrecorded });
    expect(resolve('shared')).toEqual({
      kind: 'ambiguous',
      selection: { provider: null, sessionId: 'shared' },
      count: 2,
    });
    expect(resolve('elsewhere')).toEqual({
      kind: 'outside_page',
      selection: { provider: null, sessionId: 'elsewhere' },
    });
    expect(resolve('["claude","elsewhere"]')).toEqual({
      kind: 'outside_page',
      selection: { provider: 'claude', sessionId: 'elsewhere' },
    });
  });

  it('toggles without mutating the input set', () => {
    const before = new Set(['a']);
    const after = toggleInSet(before, 'b');
    expect(before).toEqual(new Set(['a']));
    expect(after).toEqual(new Set(['a', 'b']));
    expect(toggleInSet(after, 'a')).toEqual(new Set(['b']));
  });
});
