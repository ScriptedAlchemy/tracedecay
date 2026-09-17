import type {
  AnalyticsSubagentNodeV1,
  AnalyticsSubagentTreePayloadV1,
} from '../../contracts/generated.ts';
import { subagentLabel } from './subagentTree.ts';

/**
 * The delegation topology: the daemon's pre-order subagent tree laid out as a
 * left-to-right field, one column per generation.
 *
 * This is a pure, renderer-independent transform. It takes the reading and the
 * set of bundles a reader has chosen to open, and returns stable coordinates in
 * an abstract row/column grid. Stable inputs give identical output: the same
 * payload lays out the same way on every render and every reload, so a reader
 * who left a node at one place finds it there again.
 *
 * Three rules govern what gets drawn.
 *
 *   1. Edges come from the pre-order and `depth` alone. The daemon assembled
 *      the tree; the drawing reads its shape back the way the payload's own
 *      contract says to, and never re-resolves `parent_session_id` client-side.
 *      A second assembly could disagree with the first.
 *   2. Every session is either drawn as its own mark or counted inside exactly
 *      one bundle. `drawnSessions + bundledSessions === totalSessions` on every
 *      model, so the field cannot silently drop a delegation.
 *   3. Fan-out past `FANOUT_LIMIT` under one parent is bundled by agent label
 *      rather than drawn one row per session. The bundles carry reconciled
 *      counts; opening one is a reader's explicit act, recorded in `expanded`.
 *
 * Nothing here assigns evidence grades or invents a relation. A top whose
 * parent is not in the reading is a cut edge, not a root; a top on a parent
 * cycle is a cycle; both remain visible as typed stubs rather than being drawn
 * as clean sources.
 */

/** The most children one parent may show individually before they bundle. */
export const FANOUT_LIMIT = 8;

/** One session drawn at its own position. */
export interface TopologySessionMark {
  readonly kind: 'session';
  /** `provider:session_id` — the payload's own identity. */
  readonly id: string;
  readonly label: string;
  readonly node: AnalyticsSubagentNodeV1;
  readonly generation: number;
  /** Leaf-slot row. Integers for leaves, means for parents. */
  readonly row: number;
  /** The drawn parent, or null for a top. */
  readonly parentId: string | null;
  /** Children drawn beneath this mark (sessions plus bundles). */
  readonly drawnChildren: number;
  /** Sessions beneath this mark that the depth limit folded away. Zero when
   * the children are drawn; equal to `node.descendants` when they are not. */
  readonly foldedDescendants: number;
}

/** Several sibling sessions folded into one mark. Their subtrees are counted,
 * not drawn, until the bundle is expanded. */
export interface TopologyBundleMark {
  readonly kind: 'bundle';
  readonly id: string;
  readonly label: string;
  readonly generation: number;
  readonly row: number;
  readonly parentId: string | null;
  readonly members: readonly AnalyticsSubagentNodeV1[];
  /** Direct sessions folded in — `members.length`. */
  readonly sessions: number;
  /** Sessions beneath those members, also folded in. */
  readonly descendants: number;
  /** What the members have in common. `remainder` is the fold of every group
   * that did not fit once the limit was reached. */
  readonly basis: 'agent' | 'provider' | 'remainder';
}

export type TopologyMark = TopologySessionMark | TopologyBundleMark;

/** A drawn delegation: parent session to child mark. */
export interface TopologyEdge {
  readonly id: string;
  readonly from: string;
  readonly to: string;
  /** `delegation` reaches a drawn session; `bundle` reaches a folded group. */
  readonly kind: 'delegation' | 'bundle';
  /** The tool call that delegated, when the child session recorded one. */
  readonly toolUseId: string | null;
}

/** An edge the reading names but cannot draw to a source. */
export interface TopologyStub {
  readonly id: string;
  readonly to: string;
  readonly kind: 'missing_parent' | 'cycle';
  readonly parentSessionId: string | null;
}

export interface TopologyGeneration {
  readonly generation: number;
  readonly marks: number;
  /** Sessions drawn individually in this column. */
  readonly sessions: number;
  /** Sessions folded into bundles in this column. */
  readonly bundled: number;
}

export interface DelegationTopologyModel {
  readonly marks: readonly TopologyMark[];
  readonly edges: readonly TopologyEdge[];
  readonly stubs: readonly TopologyStub[];
  readonly generations: readonly TopologyGeneration[];
  /** Leaf slots the layout occupies — the field's height in rows. */
  readonly rows: number;
  /** Columns the layout occupies — deepest drawn generation plus one. */
  readonly columns: number;
  readonly totalSessions: number;
  readonly drawnSessions: number;
  /** Sessions counted inside bundles: the folded siblings and everything
   * beneath them. */
  readonly bundledSessions: number;
  /** The widest descendant count among drawn sessions, for scaling marks. */
  readonly maxDescendants: number;
}

export function markId(node: AnalyticsSubagentNodeV1): string {
  return `${node.provider}:${node.session_id}`;
}

interface TreeIndex {
  readonly children: ReadonlyMap<number, readonly number[]>;
  readonly tops: readonly number[];
  /** Sessions beneath each position, counted over this index rather than read
   * from `descendants`, so the drawn/bundled reconciliation holds even for a
   * payload whose own counts disagree with its pre-order. */
  readonly beneath: readonly number[];
}

function countBeneath(
  count: number,
  children: ReadonlyMap<number, readonly number[]>,
): number[] {
  const beneath = new Array<number>(count).fill(0);
  // Pre-order puts every child after its parent, so a reverse pass sees each
  // subtree complete before the node that owns it.
  for (let position = count - 1; position >= 0; position -= 1) {
    const own = children.get(position);
    if (!own) continue;
    let total = 0;
    for (const child of own) total += 1 + beneath[child]!;
    beneath[position] = total;
  }
  return beneath;
}

/**
 * Parent/child positions from the pre-order.
 *
 * A node at depth d is the child of the nearest earlier node at depth d-1;
 * the daemon guarantees the flattening has that property, so a depth stack is
 * the whole reconstruction. A node whose depth skips ahead of the stack is a
 * contradiction in the payload and is filed as a top rather than guessed at.
 */
function indexTree(nodes: readonly AnalyticsSubagentNodeV1[]): TreeIndex {
  const children = new Map<number, number[]>();
  const tops: number[] = [];
  const stack: number[] = [];
  nodes.forEach((node, position) => {
    while (stack.length > node.depth) stack.pop();
    // A skipped depth leaves a hole in the stack; the hole reads as "no
    // parent" for this node and for anything that later points at it.
    const parent = node.depth === 0 ? undefined : stack[node.depth - 1];
    if (parent === undefined) {
      tops.push(position);
    } else {
      const bucket = children.get(parent);
      if (bucket) bucket.push(position);
      else children.set(parent, [position]);
    }
    stack.length = node.depth;
    stack.push(position);
  });
  return { children, tops, beneath: countBeneath(nodes.length, children) };
}

interface Group {
  readonly key: string;
  readonly label: string;
  readonly basis: 'agent' | 'provider';
  readonly members: number[];
}

/** Siblings grouped by agent label, or by provider where no agent was
 * recorded. Sorted largest first with the label as tie-break, so two reads
 * of the same payload fold identically. */
function groupSiblings(
  positions: readonly number[],
  nodes: readonly AnalyticsSubagentNodeV1[],
): Group[] {
  const groups = new Map<string, Group>();
  for (const position of positions) {
    const node = nodes[position]!;
    const basis: Group['basis'] = node.agent == null ? 'provider' : 'agent';
    const label = node.agent ?? node.provider;
    const key = `${basis}:${label}`;
    const existing = groups.get(key);
    if (existing) existing.members.push(position);
    else groups.set(key, { key, label, basis, members: [position] });
  }
  return [...groups.values()].sort(
    (a, b) => b.members.length - a.members.length || a.label.localeCompare(b.label),
  );
}

export interface TopologyOptions {
  /** Bundle ids a reader opened, and session ids whose children a reader
   * asked to see past the depth limit. */
  readonly expanded?: ReadonlySet<string>;
  /** Generations drawn before children fold into their parent's count. A
   * session at this generation draws no children unless it is in `expanded`. */
  readonly depthLimit?: number;
}

export function layoutDelegationTopology(
  payload: AnalyticsSubagentTreePayloadV1,
  options: TopologyOptions = {},
): DelegationTopologyModel {
  const expanded = options.expanded ?? new Set<string>();
  const depthLimit = options.depthLimit ?? Number.POSITIVE_INFINITY;
  const nodes = payload.nodes;
  const { children, tops, beneath } = indexTree(nodes);
  const marks: TopologyMark[] = [];
  const edges: TopologyEdge[] = [];
  const stubs: TopologyStub[] = [];
  let nextRow = 0;
  let drawnSessions = 0;
  let bundledSessions = 0;
  let maxDescendants = 0;
  let deepest = 0;

  const placeSession = (position: number, parentId: string | null): TopologySessionMark => {
    const node = nodes[position]!;
    const id = markId(node);
    const generation = node.depth;
    deepest = Math.max(deepest, generation);
    maxDescendants = Math.max(maxDescendants, node.descendants);
    drawnSessions += 1;
    const own = children.get(position) ?? [];
    const drawChildren = own.length > 0 && (generation < depthLimit || expanded.has(id));
    const childMarks = drawChildren ? placeChildren(own, id, generation + 1) : [];
    const foldedDescendants = drawChildren ? 0 : beneath[position]!;
    bundledSessions += foldedDescendants;
    const row =
      childMarks.length === 0
        ? nextRow++
        : childMarks.reduce((sum, mark) => sum + mark.row, 0) / childMarks.length;
    const mark: TopologySessionMark = {
      kind: 'session',
      id,
      label: subagentLabel(node),
      node,
      generation,
      row,
      parentId,
      drawnChildren: childMarks.length,
      foldedDescendants,
    };
    marks.push(mark);
    for (const child of childMarks) {
      edges.push({
        id: `${id}->${child.id}`,
        from: id,
        to: child.id,
        kind: child.kind === 'bundle' ? 'bundle' : 'delegation',
        toolUseId: child.kind === 'session' ? child.node.parent_tool_use_id : null,
      });
    }
    return mark;
  };

  const placeBundle = (
    parentId: string | null,
    generation: number,
    group: { key: string; label: string; basis: TopologyBundleMark['basis']; members: number[] },
  ): TopologyBundleMark => {
    const members = group.members.map((position) => nodes[position]!);
    const folded = group.members.reduce((sum, position) => sum + beneath[position]!, 0);
    bundledSessions += members.length + folded;
    deepest = Math.max(deepest, generation);
    const mark: TopologyBundleMark = {
      kind: 'bundle',
      id: `bundle:${parentId ?? 'source'}:${group.key}`,
      label: group.label,
      generation,
      row: nextRow++,
      parentId,
      members,
      sessions: members.length,
      descendants: folded,
      basis: group.basis,
    };
    marks.push(mark);
    return mark;
  };

  const placeChildren = (
    positions: readonly number[],
    parentId: string | null,
    generation: number,
  ): TopologyMark[] => {
    if (positions.length <= FANOUT_LIMIT) {
      return positions.map((position) => placeSession(position, parentId));
    }
    const groups = groupSiblings(positions, nodes);
    const placed: TopologyMark[] = [];
    const remainder: number[] = [];
    let remainderBasis = false;
    groups.forEach((group, index) => {
      const bundleId = `bundle:${parentId ?? 'source'}:${group.key}`;
      if (expanded.has(bundleId)) {
        for (const position of group.members) placed.push(placeSession(position, parentId));
        return;
      }
      // Once the column would overflow the limit, every further group folds
      // into one remainder so a wide agent population still fits one screen.
      if (index >= FANOUT_LIMIT - 1 && groups.length > FANOUT_LIMIT) {
        remainder.push(...group.members);
        remainderBasis = true;
        return;
      }
      if (group.members.length === 1) {
        placed.push(placeSession(group.members[0]!, parentId));
        return;
      }
      placed.push(placeBundle(parentId, generation, group));
    });
    if (remainderBasis && remainder.length > 0) {
      const remainderId = `bundle:${parentId ?? 'source'}:remainder`;
      if (expanded.has(remainderId)) {
        for (const position of remainder) placed.push(placeSession(position, parentId));
      } else {
        placed.push(
          placeBundle(parentId, generation, {
            key: 'remainder',
            label: `${remainder.length} more sessions`,
            basis: 'remainder',
            members: remainder,
          }),
        );
      }
    }
    return placed;
  };

  placeChildren(tops, null, 0);
  // Column-major reading order: generation left to right, row top to bottom.
  // Placement pushes in post-order, which is right for computing rows and
  // wrong for a reader walking the field.
  marks.sort((a, b) => a.generation - b.generation || a.row - b.row || a.id.localeCompare(b.id));
  const order = new Map(marks.map((mark, index) => [mark.id, index]));
  edges.sort(
    (a, b) =>
      (order.get(a.from) ?? 0) - (order.get(b.from) ?? 0) ||
      (order.get(a.to) ?? 0) - (order.get(b.to) ?? 0),
  );

  for (const mark of marks) {
    if (mark.kind !== 'session' || mark.parentId !== null) continue;
    switch (mark.node.link) {
      case 'root':
      case 'linked':
        // `linked` at a top is a payload contradiction; drawn as a plain top
        // because there is no parent to stub towards.
        break;
      case 'missing_parent':
        stubs.push({
          id: `stub:${mark.id}`,
          to: mark.id,
          kind: 'missing_parent',
          parentSessionId: mark.node.parent_session_id,
        });
        break;
      case 'cycle':
        stubs.push({
          id: `stub:${mark.id}`,
          to: mark.id,
          kind: 'cycle',
          parentSessionId: mark.node.parent_session_id,
        });
        break;
      default: {
        const unhandled: never = mark.node.link;
        return unhandled;
      }
    }
  }

  const generations: TopologyGeneration[] = [];
  for (let generation = 0; generation <= deepest && marks.length > 0; generation += 1) {
    const column = marks.filter((mark) => mark.generation === generation);
    generations.push({
      generation,
      marks: column.length,
      sessions: column.filter((mark) => mark.kind === 'session').length,
      bundled: column.reduce(
        (sum, mark) => sum + (mark.kind === 'bundle' ? mark.sessions : 0),
        0,
      ),
    });
  }

  return {
    marks,
    edges,
    stubs,
    generations,
    rows: nextRow,
    columns: marks.length === 0 ? 0 : deepest + 1,
    totalSessions: nodes.length,
    drawnSessions,
    bundledSessions,
    maxDescendants,
  };
}

/** Leaf rows the field will lay out before it folds deeper generations. Forty
 * rows at the row pitch is one tall screen; past that a reader is scrolling
 * a list drawn as a diagram, and the exact tree beneath the field is the
 * better instrument for it. */
export const ROW_BUDGET = 40;

export interface FittedTopology {
  readonly model: DelegationTopologyModel;
  /** The depth limit the fit settled on; `Infinity` when nothing was folded. */
  readonly depthLimit: number;
  /** The deepest generation the reading holds, drawn or not. */
  readonly maxDepth: number;
}

/**
 * Semantic zoom by depth: draw the whole reading if it fits the row budget,
 * otherwise fold generations from the deepest inward until it does. Bundles a
 * reader opened and sessions a reader expanded are honoured at every depth,
 * so an explicit act is never undone by the fit.
 *
 * Generation 0 is always drawn. If even the tops overflow the budget, the
 * fan-out bundling has already folded them by agent and the field shows what
 * it can; the exact tree carries the rest.
 */
export function fitDelegationTopology(
  payload: AnalyticsSubagentTreePayloadV1,
  expanded: ReadonlySet<string> = new Set(),
  rowBudget: number = ROW_BUDGET,
): FittedTopology {
  const maxDepth = payload.nodes.reduce((deepest, node) => Math.max(deepest, node.depth), 0);
  const full = layoutDelegationTopology(payload, { expanded });
  if (full.rows <= rowBudget) {
    return { model: full, depthLimit: Number.POSITIVE_INFINITY, maxDepth };
  }
  for (let depthLimit = maxDepth - 1; depthLimit >= 0; depthLimit -= 1) {
    const model = layoutDelegationTopology(payload, { expanded, depthLimit });
    if (model.rows <= rowBudget || depthLimit === 0) {
      return { model, depthLimit, maxDepth };
    }
  }
  return { model: layoutDelegationTopology(payload, { expanded, depthLimit: 0 }), depthLimit: 0, maxDepth };
}

/** Column and row pitch in CSS pixels, chosen so a 12px mono label fits
 * between marks at 100% and the field still reads at 200% browser zoom. */
export const TOPOLOGY_GEOMETRY = {
  columnPitch: 176,
  rowPitch: 40,
  padX: 88,
  padY: 36,
  minRadius: 5,
  maxRadius: 12,
  sourceRadius: 16,
  /** Room the last column's labels need to the right of their marks. */
  labelRoom: 168,
  /** Widest a column may stretch when the aperture has width to spare. */
  maxColumnPitch: 320,
} as const;

export interface TopologyGeometry {
  readonly columnPitch: number;
}

/**
 * The column pitch for an aperture of a given width: the default at minimum,
 * stretched so the drawn generations span the width when there is room, and
 * capped so two generations never sit a screen apart. Deterministic in
 * `(width, columns)`, so a resize is the only thing that moves a mark.
 */
export function columnPitchFor(width: number | null, columns: number): TopologyGeometry {
  const { columnPitch, padX, labelRoom, maxColumnPitch } = TOPOLOGY_GEOMETRY;
  if (width === null || columns <= 1) return { columnPitch };
  const usable = width - padX * 2 - labelRoom;
  const stretched = Math.floor(usable / (columns - 1));
  return { columnPitch: Math.max(columnPitch, Math.min(maxColumnPitch, stretched)) };
}

/** Pixel position of a mark in the field. */
export function markPosition(
  mark: TopologyMark,
  geometry: TopologyGeometry = TOPOLOGY_GEOMETRY,
): { x: number; y: number } {
  return {
    x: TOPOLOGY_GEOMETRY.padX + mark.generation * geometry.columnPitch,
    y: TOPOLOGY_GEOMETRY.padY + mark.row * TOPOLOGY_GEOMETRY.rowPitch,
  };
}

/** Field extent in pixels for a laid-out model. */
export function fieldSize(
  model: DelegationTopologyModel,
  geometry: TopologyGeometry = TOPOLOGY_GEOMETRY,
): { width: number; height: number } {
  return {
    width: TOPOLOGY_GEOMETRY.padX * 2 + Math.max(0, model.columns - 1) * geometry.columnPitch,
    height: TOPOLOGY_GEOMETRY.padY * 2 + Math.max(0, model.rows - 1) * TOPOLOGY_GEOMETRY.rowPitch,
  };
}

/**
 * Mark radius from what the reading measured: sessions beneath it, on a log
 * band against the widest fan-out drawn. A generation-0 root reads as the
 * source and takes the source radius; a bundle is sized by the sessions it
 * folds so an unopened group is never smaller than the sessions it hides.
 */
export function markRadius(mark: TopologyMark, maxDescendants: number): number {
  const { minRadius, maxRadius, sourceRadius } = TOPOLOGY_GEOMETRY;
  if (mark.kind === 'bundle') {
    return Math.min(maxRadius, minRadius + Math.log1p(mark.sessions) * 2);
  }
  if (
    mark.generation === 0 &&
    mark.node.link === 'root' &&
    (mark.drawnChildren > 0 || mark.foldedDescendants > 0)
  ) {
    return sourceRadius;
  }
  if (maxDescendants <= 0 || mark.node.descendants <= 0) return minRadius;
  const fraction = Math.log1p(mark.node.descendants) / Math.log1p(maxDescendants);
  return minRadius + (maxRadius - minRadius) * Math.max(0, Math.min(1, fraction));
}

/** Cubic path from one mark's trailing edge to the next mark's leading edge,
 * bending at the midpoint column so parallel delegations read as a fan. */
export function edgePath(
  from: { x: number; y: number },
  to: { x: number; y: number },
  fromRadius: number,
  toRadius: number,
): string {
  const startX = from.x + fromRadius;
  const endX = to.x - toRadius;
  const midX = startX + (endX - startX) / 2;
  return `M${startX},${from.y} C${midX},${from.y} ${midX},${to.y} ${endX},${to.y}`;
}

/** The set of mark ids on the path from a mark to its top, plus the mark's
 * drawn children — the neighbourhood hover isolates. */
export function neighbourhood(
  model: DelegationTopologyModel,
  id: string,
): ReadonlySet<string> {
  const byId = new Map(model.marks.map((mark) => [mark.id, mark]));
  const keep = new Set<string>();
  let cursor: TopologyMark | undefined = byId.get(id);
  while (cursor) {
    keep.add(cursor.id);
    cursor = cursor.parentId === null ? undefined : byId.get(cursor.parentId);
  }
  for (const edge of model.edges) {
    if (edge.from === id) keep.add(edge.to);
  }
  return keep;
}
