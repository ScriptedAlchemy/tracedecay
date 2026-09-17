import { describe, expect, it } from 'vitest';
import { summarizeChain } from './chain.ts';

describe('summarizeChain', () => {
  type ChainMessage = Parameters<typeof summarizeChain>[0][number];
  const message = (over: Partial<ChainMessage> = {}): ChainMessage => ({
    message_id: 'm1',
    role: 'assistant',
    content: 'hello',
    ordinal: 0,
    timestamp: null,
    tool_name: null,
    token_count: 4,
    token_count_provenance: 'o200k_approximate',
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

  it('preserves per-message token provenance without inventing an aggregate zero', () => {
    const summary = summarizeChain([
      message({
        message_id: 'recorded',
        token_count: 13,
        token_count_provenance: 'o200k_approximate',
      }),
      message({
        message_id: 'unknown',
        token_count: null,
        token_count_provenance: null,
      }),
    ]);

    expect(summary.steps[0]).toMatchObject({
      tokenCount: 13,
      tokenCountProvenance: 'o200k_approximate',
    });
    expect(summary.steps[1]).toMatchObject({
      tokenCount: null,
      tokenCountProvenance: null,
    });
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
