import { describe, expect, it } from 'vitest';

import type { GraphSubgraphPayloadV1 } from '../../contracts/generated.ts';
import { buildCoreSample, readStructureLocation } from './structureLens.ts';

describe('structure lens deep links', () => {
  it('keeps only a real lens position with the exact focus identity it requires', () => {
    expect(readStructureLocation(new URLSearchParams())).toEqual({
      lens: 'cortex',
      focusId: null,
    });
    expect(
      readStructureLocation(
        new URLSearchParams('structureLens=trace&structureFocus=symbol-42'),
      ),
    ).toEqual({ lens: 'trace', focusId: 'symbol-42' });
    expect(
      readStructureLocation(
        new URLSearchParams('structureLens=core&structureFocus=symbol-42'),
      ),
    ).toEqual({ lens: 'core', focusId: 'symbol-42' });
    expect(readStructureLocation(new URLSearchParams('structureLens=core'))).toEqual({
      lens: 'cortex',
      focusId: null,
    });
    expect(
      readStructureLocation(
        new URLSearchParams('structureLens=disagreement&structureFocus=symbol-42'),
      ),
    ).toEqual({ lens: 'cortex', focusId: 'symbol-42' });
  });
});

describe('CORE sample projection', () => {
  it('orders symbols by measured source lines and declares every visual omission', () => {
    const nodes = Array.from({ length: 8 }, (_, index) => ({
      id: `node-${index}`,
      kind: 'function',
      name: `symbol_${index}`,
      qualified_name: null,
      file_path: `src/file-${index}.rs`,
      start_line: 80 - index,
      end_line: 90 - index,
      start_column: 0,
      end_column: 1,
      attrs_start_line: null,
      doc: null,
      signature: null,
      visibility: null,
      is_async: null,
      branches: null,
      loops: null,
      returns: null,
      max_nesting: null,
      unsafe_blocks: null,
      unchecked_calls: null,
      assertions: null,
      updated_at: null,
      parent_id: null,
      degree: index,
      span: null,
      edge_kind: null,
      edge_line: null,
    }));
    nodes.push({
      ...nodes[0]!,
      id: 'focus-sibling',
      name: 'earlier_in_focus_file',
      start_line: 12,
      end_line: 20,
    });
    const payload: GraphSubgraphPayloadV1 = {
      seed_id: 'node-0',
      mode: 'seeded',
      nodes,
      edges: [
        {
          source: 'focus-sibling',
          target: 'node-0',
          kind: 'calls',
          line: 16,
          source_name: null,
          target_name: null,
        },
        {
          source: 'node-0',
          target: 'node-1',
          kind: 'calls',
          line: 84,
          source_name: null,
          target_name: null,
        },
      ],
      capped: { nodes: false, edges: false },
      limits: { nodes: 80, edges: 120 },
    };

    const sample = buildCoreSample(payload, 'node-0');

    expect(sample).not.toBeNull();
    if (sample === null) throw new Error('the measured focus must produce a core sample');
    expect(sample.files).toHaveLength(6);
    expect(sample.files[0]?.path).toBe('src/file-0.rs');
    expect(sample.files[0]?.nodes.map((node) => node.id)).toEqual([
      'focus-sibling',
      'node-0',
    ]);
    expect(sample.files[0]?.internalEdges).toHaveLength(1);
    expect(sample.files[0]?.externalEdges).toHaveLength(1);
    expect(sample.hiddenFileCount).toBe(2);
    expect(sample.hiddenNodeCount).toBe(2);
    expect(sample.totalFileCount).toBe(8);
    expect(sample.totalNodeCount).toBe(9);
  });

  it('does not fabricate a core when the focus has no measured file or line', () => {
    const payload: GraphSubgraphPayloadV1 = {
      seed_id: 'focus',
      mode: 'seeded',
      nodes: [
        {
          id: 'focus',
          kind: 'function',
          name: 'focus',
          qualified_name: null,
          file_path: null,
          start_line: null,
          end_line: null,
          start_column: null,
          end_column: null,
          attrs_start_line: null,
          doc: null,
          signature: null,
          visibility: null,
          is_async: null,
          branches: null,
          loops: null,
          returns: null,
          max_nesting: null,
          unsafe_blocks: null,
          unchecked_calls: null,
          assertions: null,
          updated_at: null,
          parent_id: null,
          degree: null,
          span: null,
          edge_kind: null,
          edge_line: null,
        },
      ],
      edges: [],
      capped: { nodes: false, edges: false },
      limits: { nodes: 80, edges: 120 },
    };

    expect(buildCoreSample(payload, 'focus')).toBeNull();
  });
});
