import { describe, expect, it } from 'vitest';
import { buildAdjacency, neighborsOf } from './adjacency.ts';

describe('buildAdjacency', () => {
  it('makes a drawn relation conduct in both directions', () => {
    const adjacency = buildAdjacency([{ source: 'repo:r', target: 'p1' }]);
    expect(neighborsOf(adjacency, 'p1')).toEqual(['repo:r']);
    expect(neighborsOf(adjacency, 'repo:r')).toEqual(['p1']);
  });

  it('stops one hop short of siblings that did nothing', () => {
    // The Brain's shape: two checkouts of one repository. An event in `p1`
    // reaches the hub and must stop there — `p2` is two hops away and nothing
    // happened in it.
    const adjacency = buildAdjacency([
      { source: 'repo:r', target: 'p1' },
      { source: 'repo:r', target: 'p2' },
    ]);
    expect(neighborsOf(adjacency, 'p1')).toEqual(['repo:r']);
    expect(neighborsOf(adjacency, 'p1')).not.toContain('p2');
  });

  it('records a node reached by several relations once', () => {
    const adjacency = buildAdjacency([
      { source: 'a', target: 'b' },
      { source: 'b', target: 'a' },
      { source: 'a', target: 'b' },
    ]);
    expect(neighborsOf(adjacency, 'a')).toEqual(['b']);
  });

  it('ignores a self-relation rather than letting a node hop to itself', () => {
    const adjacency = buildAdjacency([{ source: 'a', target: 'a' }]);
    expect(neighborsOf(adjacency, 'a')).toEqual([]);
  });

  it('propagates nowhere from a node with no drawn relation', () => {
    expect(neighborsOf(buildAdjacency([]), 'lonely')).toEqual([]);
  });
});
