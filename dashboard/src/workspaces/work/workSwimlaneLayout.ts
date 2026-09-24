import type { WorkDagLayout, WorkDagRelationKind } from './workDagLayout.ts';
import type { WorkTaskView } from './workProductView.ts';
import type { WorkDagReading } from './workViewsModel.ts';

/**
 * The swimlane DAG: the layered reading re-laid with lanes on y and stratum
 * depth on x, so "which workstream" and "how far down the dependency chain"
 * are the two axes a reader scans.
 *
 * Lanes come from declared data only. `milestone` is the graph's own
 * hierarchy and reads as exact. `holder` is the latest recorded handoff's
 * receiving actor, an explicit claim an agent wrote down, and a task nobody
 * handed off sits in a lane that says so rather than being given an owner.
 *
 * Within a lane and a stratum tasks keep the layered board's left-to-right
 * order, so the two renderers agree on sequence. Dependencies are routed as
 * hairlines through the gutter before the dependent's column.
 *
 * The emphasised chain is the reading's deepest declared chain: dependency
 * depth, unweighted, measured from the edges this page returned. It is not
 * the authority's effort-weighted critical path and is never labelled as one.
 */

export type SwimlaneKey = 'milestone' | 'holder';

export const SWIMLANE_GEOMETRY = {
  labelWidth: 176,
  axis: 28,
  plateWidth: 184,
  plateHeight: 44,
  columnGap: 40,
  slotGap: 8,
  lanePad: 10,
} as const;

export interface SwimlanePlate {
  readonly taskId: string;
  readonly lane: number;
  readonly depth: number;
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

export interface SwimlaneLane {
  readonly key: string;
  readonly label: string;
  readonly grade: 'exact' | 'explicit' | 'unavailable';
  readonly y: number;
  readonly height: number;
  readonly taskIds: readonly string[];
}

export interface SwimlaneEdge {
  readonly id: string;
  readonly from: string;
  readonly to: string;
  readonly kind: WorkDagRelationKind;
  readonly climb: boolean;
  readonly path: string;
}

export interface SwimlaneLayout {
  readonly lanes: readonly SwimlaneLane[];
  readonly plates: readonly SwimlanePlate[];
  readonly byId: ReadonlyMap<string, SwimlanePlate>;
  readonly edges: readonly SwimlaneEdge[];
  readonly columns: number;
  readonly width: number;
  readonly height: number;
  /** Task ids on the deepest declared chain, and the gating edges joining them. */
  readonly chain: { readonly tasks: ReadonlySet<string>; readonly edges: ReadonlySet<string>; readonly depth: number };
}

/** The swimlane field's width for a layout: it depends only on how many
 * strata the layout holds, never on which lane key is chosen. */
export function swimlaneWidth(layout: WorkDagLayout): number {
  const g = SWIMLANE_GEOMETRY;
  return g.labelWidth + layout.strata.length * (g.plateWidth + g.columnGap);
}

export const NO_HANDOFF_LANE = 'no handoff recorded';

function laneOf(task: WorkTaskView, key: SwimlaneKey): { key: string; grade: SwimlaneLane['grade'] } {
  switch (key) {
    case 'milestone':
      return { key: task.hierarchy.milestone_id, grade: 'exact' };
    case 'holder': {
      const latest = [...task.handoffs].sort((a, b) => b.handedOffAt - a.handedOffAt || a.handoffId.localeCompare(b.handoffId))[0];
      return latest === undefined ? { key: NO_HANDOFF_LANE, grade: 'unavailable' } : { key: latest.toActor, grade: 'explicit' };
    }
    default: {
      const unhandled: never = key;
      return unhandled;
    }
  }
}

export function workSwimlaneLayout(
  layout: WorkDagLayout,
  reading: WorkDagReading,
  tasks: ReadonlyMap<string, WorkTaskView>,
  key: SwimlaneKey,
): SwimlaneLayout {
  const g = SWIMLANE_GEOMETRY;
  const groups = new Map<string, { grade: SwimlaneLane['grade']; nodes: typeof layout.nodes[number][] }>();
  for (const node of layout.nodes) {
    const task = tasks.get(node.taskId);
    if (task === undefined) continue;
    const lane = laneOf(task, key);
    const group = groups.get(lane.key);
    if (group) group.nodes.push(node);
    else groups.set(lane.key, { grade: lane.grade, nodes: [node] });
  }
  // Lanes that start earlier in the chain sit higher; the name breaks ties,
  // and the explicit absence always sits last.
  const ordered = [...groups.entries()].sort(([a, ga], [b, gb]) => {
    if ((a === NO_HANDOFF_LANE) !== (b === NO_HANDOFF_LANE)) return a === NO_HANDOFF_LANE ? 1 : -1;
    const depthA = Math.min(...ga.nodes.map((node) => node.depth));
    const depthB = Math.min(...gb.nodes.map((node) => node.depth));
    return depthA - depthB || a.localeCompare(b);
  });

  const depths = [...new Set(layout.nodes.map((node) => node.depth))].sort((a, b) => a - b);
  const columnOf = new Map(depths.map((depth, index) => [depth, index]));
  const lanes: SwimlaneLane[] = [];
  const plates: SwimlanePlate[] = [];
  const byId = new Map<string, SwimlanePlate>();
  let y = g.axis;
  ordered.forEach(([laneKey, group], laneIndex) => {
    const perColumn = new Map<number, number>();
    for (const node of [...group.nodes].sort((a, b) => a.depth - b.depth || a.column - b.column)) {
      const column = columnOf.get(node.depth)!;
      const slot = perColumn.get(column) ?? 0;
      perColumn.set(column, slot + 1);
      const plate: SwimlanePlate = {
        taskId: node.taskId,
        lane: laneIndex,
        depth: node.depth,
        x: g.labelWidth + column * (g.plateWidth + g.columnGap),
        y: y + g.lanePad + slot * (g.plateHeight + g.slotGap),
        width: g.plateWidth,
        height: g.plateHeight,
      };
      plates.push(plate);
      byId.set(plate.taskId, plate);
    }
    const slots = Math.max(1, ...perColumn.values());
    const height = g.lanePad * 2 + slots * g.plateHeight + (slots - 1) * g.slotGap;
    lanes.push({
      key: laneKey,
      label: laneKey,
      grade: group.grade,
      y,
      height,
      taskIds: group.nodes.map((node) => node.taskId),
    });
    y += height;
  });

  const incoming = new Map<string, number>();
  const edges: SwimlaneEdge[] = [];
  for (const edge of layout.edges) {
    const from = byId.get(edge.from);
    const to = byId.get(edge.to);
    if (from === undefined || to === undefined) continue;
    let path: string;
    if (to.x > from.x && !edge.climb) {
      const nth = incoming.get(edge.to) ?? 0;
      incoming.set(edge.to, nth + 1);
      const sy = from.y + from.height / 2 + (edge.kind === 'gating' ? 0 : 8);
      const ey = to.y + to.height / 2 + (edge.kind === 'gating' ? 0 : 8);
      const gutter = to.x - 12 - (nth % 4) * 5;
      path = `M ${from.x + from.width} ${sy} H ${gutter} V ${ey} H ${to.x}`;
    } else {
      // Same stratum or backwards: a cycle climb or a soft relation. Arced
      // over the plates so it can never be read as a routed gating hairline.
      const sx = from.x + from.width / 2;
      const ex = to.x + to.width / 2;
      const top = Math.min(from.y, to.y) - 14;
      path = `M ${sx} ${from.y} C ${sx} ${top}, ${ex} ${top}, ${ex} ${to.y}`;
    }
    edges.push({ id: edge.id, from: edge.from, to: edge.to, kind: edge.kind, climb: edge.climb, path });
  }

  const chainTasks = new Set<string>();
  const chainEdges = new Set<string>();
  const chain = reading.longestChain;
  chain.forEach((component, index) => {
    for (const taskId of component.taskIds) chainTasks.add(taskId);
    const next = chain[index + 1];
    if (next === undefined) return;
    for (const from of component.taskIds) {
      for (const to of next.taskIds) {
        const id = `gating:${from}->${to}`;
        if (layout.edges.some((edge) => edge.id === id)) chainEdges.add(id);
      }
    }
  });

  return {
    lanes,
    plates,
    byId,
    edges,
    columns: depths.length,
    width: swimlaneWidth(layout),
    height: y + 8,
    chain: { tasks: chainTasks, edges: chainEdges, depth: chain.length },
  };
}
