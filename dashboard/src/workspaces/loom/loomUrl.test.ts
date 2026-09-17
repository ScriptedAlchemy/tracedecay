import { describe, expect, it } from 'vitest';
import {
  parseHiddenKinds,
  parseLaneSet,
  parseWindow,
  parseZoom,
  serializeHiddenKinds,
  serializeLaneSet,
  serializeWindow,
  toggleInSet,
} from './loomUrl.ts';

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

  it('toggles without mutating the input set', () => {
    const before = new Set(['a']);
    const after = toggleInSet(before, 'b');
    expect(before).toEqual(new Set(['a']));
    expect(after).toEqual(new Set(['a', 'b']));
    expect(toggleInSet(after, 'a')).toEqual(new Set(['b']));
  });
});
