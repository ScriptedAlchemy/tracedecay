import { describe, expect, it } from 'vitest';
import { annotateHubs, describeSubgraph } from './hubs.ts';

/** The twelve rows `graph/overview` served on 2026-07-25. Eight of the names
 * are language primitives or one-word generics, and two of them are the same
 * word in different files. */
const LIVE_HUBS = [
  { name: 'path', kind: 'function', file_path: 'src/dashboard/graph_api.rs', degree: 2038 },
  { name: 'path', kind: 'method', file_path: 'src/automation/skill_materialization.rs', degree: 1827 },
  {
    name: 'json',
    kind: 'function',
    file_path: 'crates/tracedecay-rusqlite-runtime/src/repair/sqlite.rs',
    degree: 1547,
  },
  { name: 'Value', kind: 'enum_variant', file_path: 'src/dashboard/code_diagnostics_api.rs', degree: 926 },
  { name: 'u64', kind: 'method', file_path: 'src/application/session/refresh.rs', degree: 810 },
  { name: 'as_str', kind: 'method', file_path: 'src/memory/types.rs', degree: 776 },
  { name: 'trim', kind: 'function', file_path: 'scripts/render-codex-hook-inputs.py', degree: 727 },
  { name: 'i64', kind: 'method', file_path: 'src/application/session/refresh.rs', degree: 637 },
  { name: 'kind', kind: 'method', file_path: 'crates/tracedecay-tool-catalog/src/profile.rs', degree: 610 },
  {
    name: 'find_direct_child_by_kind',
    kind: 'function',
    file_path: 'src/extraction/traversal.rs',
    degree: 489,
  },
  { name: 'test', kind: 'annotation_usage', file_path: 'src/branch/admin/tests.rs', degree: 468 },
  { name: 'u32', kind: 'impl', file_path: 'src/db/engine/value.rs', degree: 453 },
];

describe('annotateHubs', () => {
  it('splits each hub into module and file', () => {
    const annotated = annotateHubs(LIVE_HUBS);
    expect(annotated[0]!.module).toBe('src/dashboard/');
    expect(annotated[0]!.file).toBe('graph_api.rs');
    expect(annotated[2]!.module).toBe('crates/tracedecay-rusqlite-runtime/src/repair/');
    expect(annotated[2]!.file).toBe('sqlite.rs');
  });

  it('flags the two rows that share a name and leaves the rest alone', () => {
    const annotated = annotateHubs(LIVE_HUBS);
    expect(annotated[0]!.ambiguous).toBe(true);
    expect(annotated[1]!.ambiguous).toBe(true);
    expect(annotated.filter((row) => row.ambiguous)).toHaveLength(2);
    // `u64` and `u32` are different names, however alike they look.
    expect(annotated[4]!.ambiguous).toBe(false);
    expect(annotated[11]!.ambiguous).toBe(false);
  });

  it('handles a path with no directory and a hub with no path at all', () => {
    const annotated = annotateHubs([
      { name: 'main', file_path: 'build.rs' },
      { name: 'ghost', file_path: null },
    ]);
    expect(annotated[0]).toMatchObject({ module: '', file: 'build.rs' });
    expect(annotated[1]).toMatchObject({ module: '', file: '' });
  });

  it('falls back through the name chain the endpoint actually serves', () => {
    const annotated = annotateHubs([{ id: 'function:abc', file_path: 'a/b.rs' }]);
    expect(annotated[0]!.display).toBe('function:abc');
  });
});

describe('describeSubgraph', () => {
  const nodes = Array.from({ length: 80 }, (_, i) => ({ id: `n${i}` }));
  const edges = Array.from({ length: 120 }, (_, i) => ({ id: `e${i}` }));

  it('distinguishes a query that matched nothing from an empty graph', () => {
    expect(
      describeSubgraph({ mode: 'seeded', seed_id: null, nodes: [], edges: [] }, 100)!.rule,
    ).toContain('matched no symbol');
    expect(
      describeSubgraph({ mode: 'default', seed_id: null, nodes: [], edges: [] }, 100)!.rule,
    ).toContain('returned no slice');
  });

  it('has nothing to caption before the payload arrives', () => {
    expect(describeSubgraph(undefined, 100)).toBeNull();
  });
});
