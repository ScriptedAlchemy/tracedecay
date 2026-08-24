import type {
  GraphEdgeV1,
  GraphNodeV1,
  GraphSubgraphPayloadV1,
} from '../../contracts/generated.ts';

export type StructureLens = 'cortex' | 'trace' | 'core';

export interface StructureLocation {
  readonly lens: StructureLens;
  readonly focusId: string | null;
}

const LENS_PARAM = 'structureLens';
const FOCUS_PARAM = 'structureFocus';
const CORE_FILE_LIMIT = 6;

/** Reads only stable graph identity from a deep link. A lens that requires a
 * focus cannot exist without one, and unknown future lens names fail closed to
 * the macro field instead of being guessed into a current view. */
export function readStructureLocation(params: URLSearchParams): StructureLocation {
  const focusId = params.get(FOCUS_PARAM);
  const requested = params.get(LENS_PARAM);
  if ((requested === 'trace' || requested === 'core') && focusId !== null) {
    return { lens: requested, focusId };
  }
  return { lens: 'cortex', focusId };
}

export function writeStructureLocation(
  current: URLSearchParams,
  location: StructureLocation,
): URLSearchParams {
  const next = new URLSearchParams(current);
  if (location.lens === 'cortex') next.delete(LENS_PARAM);
  else next.set(LENS_PARAM, location.lens);
  if (location.focusId === null) next.delete(FOCUS_PARAM);
  else next.set(FOCUS_PARAM, location.focusId);
  return next;
}

export interface CoreFileSample {
  readonly path: string;
  readonly nodes: readonly GraphNodeV1[];
  readonly internalEdges: readonly GraphEdgeV1[];
  readonly externalEdges: readonly GraphEdgeV1[];
  readonly minLine: number;
  readonly maxLine: number;
}

export interface CoreSample {
  readonly focusId: string;
  readonly files: readonly CoreFileSample[];
  readonly rows: readonly GraphNodeV1[];
  readonly totalFileCount: number;
  readonly hiddenFileCount: number;
  readonly totalNodeCount: number;
  readonly hiddenNodeCount: number;
}

function hasSourcePosition(
  node: GraphNodeV1,
): node is GraphNodeV1 & { file_path: string; start_line: number; end_line: number } {
  return (
    typeof node.file_path === 'string' &&
    node.file_path.length > 0 &&
    typeof node.start_line === 'number' &&
    typeof node.end_line === 'number'
  );
}

/** Builds the six-column CORE sample strictly from the returned subgraph.
 * Source lines, file membership, and call relations are copied from the wire;
 * the browser chooses only presentation order and reports the omitted counts. */
export function buildCoreSample(
  payload: GraphSubgraphPayloadV1,
  focusId: string,
): CoreSample | null {
  const positioned = payload.nodes.filter(hasSourcePosition);
  const focus = positioned.find((node) => node.id === focusId);
  if (focus === undefined) return null;

  const nodesByFile = new Map<string, GraphNodeV1[]>();
  const fileByNode = new Map<string, string>();
  for (const node of positioned) {
    const path = node.file_path;
    const fileNodes = nodesByFile.get(path) ?? [];
    fileNodes.push(node);
    nodesByFile.set(path, fileNodes);
    fileByNode.set(node.id, path);
  }

  const focusPath = focus.file_path;
  const orderedPaths = [...nodesByFile.keys()].sort((left, right) => {
    if (left === focusPath) return -1;
    if (right === focusPath) return 1;
    const byMass = (nodesByFile.get(right)?.length ?? 0) - (nodesByFile.get(left)?.length ?? 0);
    return byMass === 0 ? left.localeCompare(right) : byMass;
  });
  const visiblePaths = orderedPaths.slice(0, CORE_FILE_LIMIT);
  const calls = payload.edges.filter((edge) => edge.kind === 'calls');

  const files = visiblePaths.map((path): CoreFileSample => {
    const nodes = [...(nodesByFile.get(path) ?? [])].sort(
      (left, right) =>
        (left.start_line ?? 0) - (right.start_line ?? 0) || left.id.localeCompare(right.id),
    );
    const internalEdges: GraphEdgeV1[] = [];
    const externalEdges: GraphEdgeV1[] = [];
    for (const edge of calls) {
      const sourceFile = fileByNode.get(edge.source);
      const targetFile = fileByNode.get(edge.target);
      if (sourceFile === path && targetFile === path) internalEdges.push(edge);
      else if (
        (sourceFile === path && targetFile !== undefined) ||
        (targetFile === path && sourceFile !== undefined)
      ) {
        externalEdges.push(edge);
      }
    }
    return {
      path,
      nodes,
      internalEdges,
      externalEdges,
      minLine: nodes[0]?.start_line ?? 0,
      maxLine: nodes.reduce((max, node) => Math.max(max, node.end_line ?? max), 0),
    };
  });

  const hiddenPaths = orderedPaths.slice(CORE_FILE_LIMIT);
  return {
    focusId,
    files,
    rows: [...positioned].sort(
      (left, right) =>
        (left.file_path ?? '').localeCompare(right.file_path ?? '') ||
        (left.start_line ?? 0) - (right.start_line ?? 0) ||
        left.id.localeCompare(right.id),
    ),
    totalFileCount: orderedPaths.length,
    hiddenFileCount: hiddenPaths.length,
    totalNodeCount: positioned.length,
    hiddenNodeCount: hiddenPaths.reduce(
      (total, path) => total + (nodesByFile.get(path)?.length ?? 0),
      0,
    ),
  };
}
