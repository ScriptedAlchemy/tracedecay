import { describe, expect, it } from 'vitest';
import {
  composeWeave,
  extentOf,
  summarizeChain,
  threadsFrom,
  type WeaveSession,
} from './weave.ts';

/** A wire-shaped session row. Defaults mirror the real payload: a start, a
 * message count, and NO end — which is the majority case on the real profile. */
function session(overrides: Partial<WeaveSession> = {}): WeaveSession {
  return {
    session_id: 'sess-1',
    provider: 'cursor',
    title: null,
    started_at: 1_784_700_000,
    last_message_at: null,
    messages: 10,
    is_subagent: false,
    models: [],
    ...overrides,
  };
}

describe('threadsFrom', () => {
  it('keeps a served end only when it is later than the start', () => {
    const { threads } = threadsFrom([
      session({ session_id: 'a', last_message_at: 1_784_700_600 }),
      // Same instant recorded twice is not a duration.
      session({ session_id: 'b', last_message_at: 1_784_700_000 }),
      session({ session_id: 'c', last_message_at: null }),
    ]);
    expect(threads.map((thread) => thread.end)).toEqual([1_784_700_600, null, null]);
  });

  it('drops rows with no usable start rather than inventing a position', () => {
    const { threads, undated } = threadsFrom([
      session({ session_id: 'a' }),
      session({ session_id: 'b', started_at: 0 }),
      session({ session_id: 'c', started_at: Number.NaN }),
    ]);
    expect(threads).toHaveLength(1);
    expect(undated).toBe(2);
  });

  it('labels a thread by title when there is one and by id when there is not', () => {
    const { threads } = threadsFrom([
      session({ session_id: 'a', title: 'Verify QUERY scheduler' }),
      session({ session_id: 'b', title: '   ' }),
    ]);
    expect(threads[0]?.label).toBe('Verify QUERY scheduler');
    expect(threads[1]?.label).toBe('b');
  });

  it('keeps equal session ids from different providers independently selectable', () => {
    const { threads } = threadsFrom([
      session({ provider: 'cursor', session_id: 'shared' }),
      session({ provider: 'claude', session_id: 'shared' }),
    ]);
    expect(new Set(threads.map((thread) => thread.id)).size).toBe(2);
    expect(threads.map((thread) => thread.sessionId)).toEqual(['shared', 'shared']);
  });

  it('collects distinct model names and ignores the null placeholder', () => {
    const { threads } = threadsFrom([
      session({
        models: [
          { model: null },
          { model: 'gpt-5.6-sol-high' },
          { model: 'gpt-5.6-sol-high' },
          { model: 'composer-2.5-fast' },
        ],
      }),
    ]);
    expect(threads[0]?.models).toEqual(['gpt-5.6-sol-high', 'composer-2.5-fast']);
  });

  it('treats a zero message count as a reading, not a missing value', () => {
    const { threads } = threadsFrom([session({ messages: 0 })]);
    expect(threads[0]?.messages).toBe(0);
  });
});

describe('extentOf', () => {
  it('spans starts and served ends together', () => {
    const { threads } = threadsFrom([
      session({ session_id: 'a', started_at: 1_000_000 }),
      session({
        session_id: 'b',
        started_at: 1_100_000,
        last_message_at: 1_200_000,
      }),
    ]);
    expect(extentOf(threads)).toEqual({ start: 1_000_000, end: 1_200_000 });
  });

  it('gives a single instant an hour of axis rather than a zero span', () => {
    const { threads } = threadsFrom([session({ started_at: 1_000_000 })]);
    expect(extentOf(threads)).toEqual({ start: 1_000_000, end: 1_003_600 });
  });

  it('is null when nothing is placeable', () => {
    expect(extentOf([])).toBeNull();
  });
});

describe('composeWeave', () => {
  it('orders host columns by how much of the weave each carries, then by name', () => {
    const weave = composeWeave([
      session({ session_id: 'a', provider: 'codex' }),
      session({ session_id: 'b', provider: 'cursor' }),
      session({ session_id: 'c', provider: 'cursor' }),
      session({ session_id: 'd', provider: 'claude' }),
    ]);
    expect(weave.hosts.map((host) => host.id)).toEqual(['cursor', 'claude', 'codex']);
    expect(weave.hosts[0]?.count).toBe(2);
  });

  it('places every thread in its own host column', () => {
    const weave = composeWeave([
      session({ session_id: 'a', provider: 'cursor' }),
      session({ session_id: 'b', provider: 'codex' }),
    ]);
    for (const thread of weave.threads) {
      expect(weave.hosts[thread.column]?.id).toBe(thread.host);
    }
  });

  it('packs overlapping threads into separate sub-columns and reuses a free one', () => {
    const weave = composeWeave([
      session({ session_id: 'a', started_at: 100, last_message_at: 200 }),
      // Overlaps a — needs its own lane.
      session({ session_id: 'b', started_at: 150, last_message_at: 250 }),
      // Starts after a ended — may reuse a's lane.
      session({ session_id: 'c', started_at: 300, last_message_at: 400 }),
    ]);
    const lane = (id: string) =>
      weave.threads.find((thread) => thread.sessionId === id)?.lane;
    expect(lane('a')).toBe(0);
    expect(lane('b')).toBe(1);
    expect(lane('c')).toBe(0);
    expect(weave.hosts[0]?.lanes).toBe(2);
  });

  it('scales width logarithmically against the heaviest thread', () => {
    const weave = composeWeave([
      session({ session_id: 'a', messages: 1000 }),
      session({ session_id: 'b', messages: 10 }),
    ]);
    const heavy = weave.threads.find((thread) => thread.sessionId === 'a');
    const light = weave.threads.find((thread) => thread.sessionId === 'b');
    expect(heavy?.weight).toBe(1);
    expect(light?.weight).toBeGreaterThan(0);
    expect(light?.weight).toBeLessThan(0.5);
    // Log, not linear: a 100x heavier thread is nowhere near 100x the width.
    expect(light?.weight).toBeGreaterThan(0.01);
    expect(weave.messageCeiling).toBe(1000);
  });

  it('marks unmeasured extent and zero-message readings separately', () => {
    const weave = composeWeave([
      session({ session_id: 'a', last_message_at: 1_784_700_600 }),
      session({ session_id: 'b', last_message_at: null }),
      session({ session_id: 'c', messages: 0 }),
    ]);
    expect(weave.openEndedCount).toBe(2);
    expect(weave.hollowCount).toBe(1);
    const open = weave.threads.find((thread) => thread.sessionId === 'b');
    expect(open?.openEnded).toBe(true);
    expect(open?.hollow).toBe(false);
    const hollow = weave.threads.find((thread) => thread.sessionId === 'c');
    expect(hollow?.hollow).toBe(true);
    expect(hollow?.weight).toBe(0);
  });

  it('is deterministic — the same rows in any order compose identically', () => {
    const rows = [
      session({ session_id: 'a', provider: 'cursor', started_at: 100 }),
      session({ session_id: 'b', provider: 'codex', started_at: 200 }),
      session({ session_id: 'c', provider: 'cursor', started_at: 300 }),
    ];
    const forward = composeWeave(rows);
    const reversed = composeWeave([...rows].reverse());
    expect(reversed.threads).toEqual(forward.threads);
    expect(reversed.hosts).toEqual(forward.hosts);
  });

  it('sorts threads earliest first so the table runs forward in time', () => {
    const weave = composeWeave([
      session({ session_id: 'late', started_at: 900 }),
      session({ session_id: 'early', started_at: 100 }),
    ]);
    expect(weave.threads.map((thread) => thread.sessionId)).toEqual(['early', 'late']);
  });

  it('composes an empty payload without an extent instead of throwing', () => {
    const weave = composeWeave([]);
    expect(weave.threads).toEqual([]);
    expect(weave.hosts).toEqual([]);
    expect(weave.extent).toBeNull();
  });
});

describe('summarizeChain', () => {
  const message = (over: Record<string, unknown> = {}) => ({
    message_id: 'm1',
    role: 'assistant',
    content: 'hello',
    ordinal: 0,
    timestamp: null,
    tool_name: null,
    token_estimate: 4,
    ...over,
  });

  it('orders by the store ordinal, not by wire order', () => {
    const summary = summarizeChain([
      message({ message_id: 'c', ordinal: 2 }),
      message({ message_id: 'a', ordinal: 0 }),
      message({ message_id: 'b', ordinal: 1 }),
    ]);
    expect(summary.steps.map((step) => step.id)).toEqual(['a', 'b', 'c']);
  });

  it('falls back to wire order when ordinals are absent', () => {
    const summary = summarizeChain([
      message({ message_id: 'x', ordinal: null }),
      message({ message_id: 'y', ordinal: null }),
    ]);
    expect(summary.steps.map((step) => step.id)).toEqual(['x', 'y']);
  });

  it('counts roles and tools, ranked by frequency', () => {
    const summary = summarizeChain([
      message({ message_id: '1', role: 'user', ordinal: 0 }),
      message({ message_id: '2', role: 'assistant', tool_name: 'Read', ordinal: 1 }),
      message({ message_id: '3', role: 'assistant', tool_name: 'Read', ordinal: 2 }),
      message({ message_id: '4', role: 'assistant', tool_name: 'Bash', ordinal: 3 }),
    ]);
    expect(summary.roles).toEqual([
      { role: 'assistant', count: 3 },
      { role: 'user', count: 1 },
    ]);
    expect(summary.tools).toEqual([
      { tool: 'Read', count: 2 },
      { tool: 'Bash', count: 1 },
    ]);
  });

  it('reports that no turn carried a timestamp — the real-profile case', () => {
    const summary = summarizeChain([message({ timestamp: null })]);
    expect(summary.timestamped).toBe(false);
  });

  it('reports timestamps when the store does serve them', () => {
    const summary = summarizeChain([message({ timestamp: 1_784_700_000 })]);
    expect(summary.timestamped).toBe(true);
  });

  it('prefers the store total over the page length so truncation is visible', () => {
    const summary = summarizeChain([message()], { message_count: 998 }, true);
    expect(summary.messageCount).toBe(998);
    expect(summary.steps).toHaveLength(1);
    expect(summary.truncated).toBe(true);
  });

  it('collapses whitespace and truncates an excerpt', () => {
    const summary = summarizeChain([
      message({ content: `  a\n\n   b  ` }),
      message({ message_id: 'long', content: 'x'.repeat(400), ordinal: 1 }),
    ]);
    expect(summary.steps[0]?.excerpt).toBe('a b');
    expect(summary.steps[1]?.excerpt).toHaveLength(140);
    expect(summary.steps[1]?.excerpt.endsWith('…')).toBe(true);
  });
});
