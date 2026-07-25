import { describe, expect, it } from 'vitest';
import { ambiguityNote, annotateHubs, describeSubgraph } from './hubs.ts';

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

describe('ambiguityNote', () => {
  it('names the repeats and scopes the claim to the set', () => {
    const note = ambiguityNote(annotateHubs(LIVE_HUBS))!;
    expect(note).toContain('2 of these 12 share a name');
    expect(note).toContain('2×path');
    // The claim must never reach past the twelve rows the payload carries.
    expect(note).not.toMatch(/118|graph|everywhere/);
  });

  it('says nothing when every name is unique', () => {
    expect(ambiguityNote(annotateHubs(LIVE_HUBS.slice(1, 5)))).toBeNull();
  });
});

describe('describeSubgraph', () => {
  const nodes = Array.from({ length: 80 }, (_, i) => ({ id: `n${i}` }));
  const edges = Array.from({ length: 120 }, (_, i) => ({ id: `e${i}` }));

  it('states the unseeded rule, which is not "the top 80 by degree"', () => {
    const caption = describeSubgraph(
      {
        mode: 'default',
        seed_id: null,
        nodes,
        edges,
        capped: { nodes: true, edges: true },
        limits: { nodes: 80, edges: 120 },
      },
      118_672,
    )!;
    expect(caption.scale).toBe('80 of 118,672 symbols · 120 edges');
    expect(caption.rule).toContain('grown by adjacency');
    expect(caption.rule).toContain('not the top 80 by degree');
    expect(caption.capped).toBe(true);
  });

  it('states the seeded rule as depth one, and names the seed', () => {
    const caption = describeSubgraph(
      {
        mode: 'seeded',
        seed_id: 'function:abc',
        nodes: nodes.slice(0, 31),
        edges: edges.slice(0, 44),
        capped: { nodes: false, edges: false },
        limits: { nodes: 80, edges: 120 },
      },
      118_672,
      'compose_registry_field',
    )!;
    expect(caption.scale).toBe('31 of 118,672 symbols · 44 edges');
    expect(caption.rule).toContain('compose_registry_field and its direct neighbours');
    expect(caption.rule).toContain('one edge away');
    expect(caption.capped).toBe(false);
  });

  it('distinguishes a query that matched nothing from an empty graph', () => {
    expect(
      describeSubgraph({ mode: 'seeded', seed_id: null, nodes: [], edges: [] }, 100)!.rule,
    ).toContain('matched no symbol');
    expect(
      describeSubgraph({ mode: 'default', seed_id: null, nodes: [], edges: [] }, 100)!.rule,
    ).toContain('returned no slice');
  });

  it('omits the total rather than inventing one when the overview has not landed', () => {
    const caption = describeSubgraph({ mode: 'default', nodes, edges }, null)!;
    expect(caption.scale).toBe('80 symbols · 120 edges');
  });

  it('has nothing to caption before the payload arrives', () => {
    expect(describeSubgraph(undefined, 100)).toBeNull();
  });
});
