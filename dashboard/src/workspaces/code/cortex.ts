/**
 * The Cortex lens's readings, as pure functions over wire payloads.
 *
 * Everything the register strip, the field legends and the inspector print is
 * derived here and nowhere else, so a figure on the glass can be traced to the
 * one rule that produced it. Nothing here fetches, guesses, or upgrades an
 * absence into a zero: every reading that can be missing is a tagged union
 * whose absent arm names why.
 */
import type {
  GraphEdgeV1,
  GraphKindCountV1,
  GraphNodeV1,
  GraphTotalsV1,
  StrataMeasurementV1,
} from '../../contracts/generated.ts';
import type { Diagnostic, DiagnosticsSnapshot } from '../../data/query/codeDiagnostics.ts';
import { directoryOf } from './cortexRelief.ts';

/* ---- register strip ------------------------------------------------------ */

/** One cell of the register: a measured value, or a named absence. */
export type RegisterReading =
  | { kind: 'measured'; value: string; unit?: string; note: string }
  | { kind: 'absent'; reason: string };

export interface RegisterCell {
  readonly label: string;
  readonly reading: RegisterReading;
}

/** The kind the index files module symbols under. The register reads that
 * kind's count as its module figure rather than inventing a directory count. */
const MODULE_KIND = 'module';

/** Module symbols in the index: the `module` entry of the kind composition. An
 * index without that kind has no module figure, and says so. */
export function moduleReading(nodesByKind: readonly GraphKindCountV1[]): RegisterReading {
  const entry = nodesByKind.find((row) => row.kind === MODULE_KIND);
  if (!entry) {
    return { kind: 'absent', reason: `no ${MODULE_KIND} kind in this index` };
  }
  return {
    kind: 'measured',
    value: entry.count.toLocaleString(),
    note: `symbols of kind ${MODULE_KIND}`,
  };
}

/** Directed edge density over the whole index: edges ÷ (nodes × (nodes − 1)).
 * Undefined below two nodes, and stated as such. */
export function densityReading(totals: GraphTotalsV1): RegisterReading {
  if (totals.nodes < 2) {
    return { kind: 'absent', reason: 'undefined below two symbols' };
  }
  const fraction = totals.edges / (totals.nodes * (totals.nodes - 1));
  const percent = fraction * 100;
  // Two significant figures below one percent, so a sparse real index reads
  // as `0.0069` rather than as a zero or an exponent.
  const value = percent >= 1 ? percent.toFixed(2) : String(Number(percent.toPrecision(2)));
  return {
    kind: 'measured',
    value,
    unit: '%',
    note: 'edges ÷ ordered pairs',
  };
}

/**
 * The seven cells of the register, in order. Nodes, edges and files are the
 * served totals; modules and density derive from them; layout and rank name
 * the drawing rule the field beneath actually uses, which is a property of the
 * renderer and not of the read.
 */
export function cortexRegister(payload: {
  totals: GraphTotalsV1;
  nodes_by_kind: readonly GraphKindCountV1[];
}): RegisterCell[] {
  const { totals } = payload;
  return [
    {
      label: 'nodes',
      reading: {
        kind: 'measured',
        value: totals.nodes.toLocaleString(),
        note: `${totals.nodes.toLocaleString()} symbols indexed`,
      },
    },
    {
      label: 'edges',
      reading: {
        kind: 'measured',
        value: totals.edges.toLocaleString(),
        note: 'relations, every kind',
      },
    },
    {
      label: 'files',
      reading: {
        kind: 'measured',
        value: totals.files.toLocaleString(),
        note: 'files with symbols',
      },
    },
    { label: 'modules', reading: moduleReading(payload.nodes_by_kind) },
    { label: 'density', reading: densityReading(totals) },
    {
      label: 'layout',
      reading: {
        kind: 'measured',
        value: 'force-directed',
        note: 'ForceAtlas2, settled once',
      },
    },
    {
      label: 'rank',
      reading: {
        kind: 'measured',
        value: 'degree',
        note: 'size = in + out edges',
      },
    },
  ];
}

/* ---- field legends ------------------------------------------------------- */

export interface LegendEntry {
  readonly kind: string;
  readonly count: number;
}

/** Symbol kinds present on the drawn slice, most numerous first. The legend
 * lists what is drawn, not every kind the index knows. */
export function kindLegend(nodes: readonly { kind: string }[]): LegendEntry[] {
  return countByKind(nodes);
}

/** Relation kinds present on the drawn slice, most numerous first. */
export function relationLegend(edges: readonly { kind: string }[]): LegendEntry[] {
  return countByKind(edges);
}

function countByKind(rows: readonly { kind: string }[]): LegendEntry[] {
  const counts = new Map<string, number>();
  for (const row of rows) counts.set(row.kind, (counts.get(row.kind) ?? 0) + 1);
  return [...counts]
    .map(([kind, count]) => ({ kind, count }))
    .sort((a, b) => b.count - a.count || a.kind.localeCompare(b.kind));
}

/* ---- relationships ------------------------------------------------------- */

/** One distinct neighbour on one side of the selected symbol, with the call
 * sites the wire listed for the pair. */
export interface DistinctNeighbor {
  readonly node: GraphNodeV1;
  /** Call sites between this neighbour and the selection: one wire row each. */
  readonly sites: number;
  /** The lines those sites sit on, in wire order, where the wire gave them. */
  readonly lines: readonly number[];
}

export interface RelationSide {
  readonly neighbors: readonly DistinctNeighbor[];
  /** Distinct neighbours listed. */
  readonly distinct: number;
  /** Call sites listed across them. */
  readonly sites: number;
  /**
   * The wire filled its row budget, so both counts above are floors: more
   * sites, and possibly more neighbours, may exist beyond the cut.
   */
  readonly capped: boolean;
  readonly limit: number;
}

/**
 * `neighbors_payload` emits ONE ROW PER CALL EDGE: a caller with four call
 * sites appears four times with the same node columns and a different
 * `edge_line`. The inspector lists each neighbour once and counts its sites,
 * and never prints a decimal beside the pair, the count of sites is the
 * whole of what the wire knows about the strength of the relation.
 */
export function groupNeighbors(rows: readonly GraphNodeV1[], limit: number): RelationSide {
  const byId = new Map<string, { node: GraphNodeV1; sites: number; lines: number[] }>();
  for (const row of rows) {
    const entry = byId.get(row.id);
    if (entry) {
      entry.sites += 1;
      if (row.edge_line != null) entry.lines.push(row.edge_line);
    } else {
      byId.set(row.id, {
        node: row,
        sites: 1,
        lines: row.edge_line != null ? [row.edge_line] : [],
      });
    }
  }
  const neighbors = [...byId.values()].sort(
    (a, b) => b.sites - a.sites || displayName(a.node).localeCompare(displayName(b.node)),
  );
  return {
    neighbors,
    distinct: neighbors.length,
    sites: rows.length,
    capped: rows.length >= limit && limit > 0,
    limit,
  };
}

export interface RelationBetween {
  readonly kind: string;
  /** `out`: the inspected symbol is the source; `in`: it is the target. */
  readonly direction: 'out' | 'in';
  readonly line: number | null;
}

/** Every drawn relation between an inspected symbol and the pinned one, so a
 * hover preview can name how the two are connected without another read. */
export function relationsBetween(
  edges: readonly GraphEdgeV1[],
  inspectedId: string,
  pinnedId: string,
): RelationBetween[] {
  if (inspectedId === pinnedId) return [];
  const out: RelationBetween[] = [];
  for (const edge of edges) {
    if (edge.source === inspectedId && edge.target === pinnedId) {
      out.push({ kind: edge.kind, direction: 'out', line: edge.line });
    } else if (edge.source === pinnedId && edge.target === inspectedId) {
      out.push({ kind: edge.kind, direction: 'in', line: edge.line });
    }
  }
  return out;
}

/* ---- strata -------------------------------------------------------------- */

export type StrataReading =
  | {
      kind: 'measured';
      depth: number;
      maxDepth: number;
      idealDepth: number;
      directory: string;
      sccSize: number;
      /** The depth is a floor: the scan stopped at its budget. */
      capped: boolean;
    }
  | {
      /** The scan laid out files in this symbol's directory but not this file. */
      kind: 'directory_only';
      directory: string;
      depths: readonly number[];
      capped: boolean;
    }
  | { kind: 'not_in_scan'; filesLaidOut: number; capped: boolean }
  | { kind: 'no_path' };

/** Where a symbol's file sits in the dependency layering, by exact path, then
 * by the directory the producer clustered on; otherwise the absence is named
 * rather than the symbol placed at depth zero. */
export function strataForPath(
  measurement: StrataMeasurementV1,
  filePath: string | null | undefined,
): StrataReading {
  if (!filePath) return { kind: 'no_path' };
  const { scan } = measurement;
  const capped =
    scan.files_examined >= scan.max_files ||
    scan.dependency_edges_examined >= scan.max_dependency_edges;
  const exact = measurement.files.find((file) => file.path === filePath);
  if (exact) {
    return {
      kind: 'measured',
      depth: exact.depth,
      maxDepth: measurement.max_depth,
      idealDepth: measurement.ideal_depth,
      directory: directoryOf(exact.path),
      sccSize: exact.scc_size,
      capped,
    };
  }
  const directory = directoryOf(filePath);
  const siblings = measurement.files.filter((file) => directoryOf(file.path) === directory);
  if (siblings.length > 0) {
    return {
      kind: 'directory_only',
      directory,
      depths: [...new Set(siblings.map((file) => file.depth))].sort((a, b) => a - b),
      capped,
    };
  }
  return { kind: 'not_in_scan', filesLaidOut: measurement.files.length, capped };
}

/* ---- diagnostics --------------------------------------------------------- */

export interface FileDiagnostics {
  readonly rows: readonly Diagnostic[];
  readonly errors: number;
  readonly warnings: number;
}

/** The broker's diagnostics that sit in one file. Matched on the path the
 * broker reported, by equality or by the graph path ending the broker's,
 * the broker may report an absolute path where the index holds a relative one. */
export function diagnosticsForFile(
  snapshot: DiagnosticsSnapshot,
  filePath: string | null | undefined,
): FileDiagnostics | null {
  if (!filePath) return null;
  const rows = snapshot.diagnostics.filter(
    (row) => row.file === filePath || row.file.endsWith(`/${filePath}`),
  );
  return {
    rows,
    errors: rows.filter((row) => row.severity === 'error').length,
    warnings: rows.filter((row) => row.severity === 'warning').length,
  };
}

/* ---- identity ------------------------------------------------------------ */

export function displayName(node: {
  name?: string | null;
  qualified_name?: string | null;
  id: string;
}): string {
  return node.name ?? node.qualified_name ?? node.id;
}

/** `file:start–end`, or as much of it as the node carries. */
export function locationLabel(node: {
  file_path?: string | null;
  start_line?: number | null;
  end_line?: number | null;
}): string | null {
  if (!node.file_path) return null;
  if (node.start_line == null) return node.file_path;
  const range =
    node.end_line != null && node.end_line !== node.start_line
      ? `${node.start_line}–${node.end_line}`
      : String(node.start_line);
  return `${node.file_path}:${range}`;
}
