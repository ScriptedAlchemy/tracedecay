/**
 * Layout of the temporal execution field: a `JourneyProjection` plus a
 * viewport becomes a `TemporalSceneModel` the renderer draws verbatim.
 *
 * Time is X, hierarchy is Y. Everything here is arithmetic over the projection
 * and the options: no clock, no randomness, no DOM. Collapsing, focus,
 * withholding and culling change what is drawn and are counted so the surface
 * can say what it left out; they never change the projection.
 */
import { axisTicks, fittedWindow } from '../../workspaces/loom/tracks.ts';
import type {
  EvidenceGrade,
  FocusTreatment,
  JourneyEvent,
  JourneyExtent,
  JourneyLane,
  JourneyProjection,
  LaneEndSource,
  LayoutOptions,
  MinimapBin,
  SceneCluster,
  SceneCursor,
  SceneGap,
  SceneInterval,
  SceneLabel,
  SceneLane,
  SceneMinimap,
  SceneNode,
  ScenePath,
  SceneRail,
  SceneTick,
  SceneViewport,
  SceneWindow,
  SemanticZoom,
  TemporalSceneModel,
  XBasis,
} from './types.ts';

export const DEFAULT_DENSE_LANE_THRESHOLD = 48;
export const RULER_HEIGHT = 36;

/** An open-ended lane draws a short tail past its start, as the weave does. */
const OPEN_TAIL_PX = 18;
const RAIL_GAP_PX = 10;
const BOTTOM_PAD_PX = 16;
const SEQUENCE_GUTTER_MIN_PX = 120;
const SEQUENCE_GUTTER_RISE_PX = 14;
const SEQUENCE_GUTTER_ROW_PX = 22;
const HALF_HIT_MAX_PX = 22;
const HALF_HIT_MIN_PX = 0.5;
const LABEL_CHAR_PX = 7;
const LABEL_PAD_PX = 6;
const LABEL_DROP_PX = 18;
const LABEL_LANE_X = 8;
const MINIMAP_HEIGHT = 64;
const MINIMAP_BINS = 96;
const MINIMAP_LANE_TOP = 6;
const MINIMAP_LANE_SPAN = 52;
const DEFAULT_WINDOW: SceneWindow = { start: 0, end: 3600 };

export function fittedWindowFor(extent: JourneyExtent | null): SceneWindow {
  return extent === null ? DEFAULT_WINDOW : fittedWindow(extent);
}

function axisSpan(viewport: SceneViewport): number {
  return Math.max(viewport.width - viewport.left - viewport.right, 1);
}

export function timeToX(viewport: SceneViewport, time: number): number {
  const duration = viewport.window.end - viewport.window.start;
  if (duration <= 0) return viewport.left;
  return viewport.left + ((time - viewport.window.start) / duration) * axisSpan(viewport);
}

export function xToTime(viewport: SceneViewport, x: number): number {
  const duration = viewport.window.end - viewport.window.start;
  if (duration <= 0) return viewport.window.start;
  return viewport.window.start + ((x - viewport.left) / axisSpan(viewport)) * duration;
}

function extentGrade(endSource: LaneEndSource): EvidenceGrade {
  switch (endSource) {
    case 'session_end':
      return 'exact';
    case 'last_message':
      return 'inferred';
    case null:
      return 'unavailable';
    default: {
      const unhandled: never = endSource;
      return unhandled;
    }
  }
}

function rowHeight(zoom: SemanticZoom, expanded: boolean, undatedEvents: number): number {
  switch (zoom) {
    case 'workstream':
      return 16;
    case 'agent':
      return 30;
    case 'event':
      if (!expanded) return 22;
      return 112 + (undatedEvents > 0 ? SEQUENCE_GUTTER_ROW_PX : 0);
    default: {
      const unhandled: never = zoom;
      return unhandled;
    }
  }
}

function compareStrings(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function tally(grades: Partial<Record<EvidenceGrade, number>>, grade: EvidenceGrade): void {
  grades[grade] = (grades[grade] ?? 0) + 1;
}

interface PendingNode {
  event: JourneyEvent;
  xBasis: XBasis;
  /** Time-based pixel, or NaN until the sequence gutter is laid out. */
  x: number;
}

/** Nearest neighbour half-gaps per (lane, y) so hit regions never overlap. */
function halfHitsFor(nodes: readonly { x: number; y: number }[]): number[] {
  const byRow = new Map<number, number[]>();
  nodes.forEach((node, index) => {
    const bucket = byRow.get(node.y);
    if (bucket) bucket.push(index);
    else byRow.set(node.y, [index]);
  });
  const out = new Array<number>(nodes.length).fill(HALF_HIT_MAX_PX);
  for (const indices of byRow.values()) {
    const sorted = [...indices].sort((a, b) => (nodes[a]?.x ?? 0) - (nodes[b]?.x ?? 0));
    sorted.forEach((index, position) => {
      const x = nodes[index]?.x ?? 0;
      let gap = Infinity;
      const before = sorted[position - 1];
      const after = sorted[position + 1];
      if (before !== undefined) gap = Math.min(gap, Math.abs(x - (nodes[before]?.x ?? 0)));
      if (after !== undefined) gap = Math.min(gap, Math.abs((nodes[after]?.x ?? 0) - x));
      out[index] = Number.isFinite(gap)
        ? Math.min(HALF_HIT_MAX_PX, Math.max(HALF_HIT_MIN_PX, gap / 2))
        : HALF_HIT_MAX_PX;
    });
  }
  return out;
}

function resolveLabels(candidates: readonly SceneLabel[]): SceneLabel[] {
  const groups = new Map<string, SceneLabel[]>();
  for (const label of candidates) {
    const bucket = groups.get(label.group);
    if (bucket) bucket.push(label);
    else groups.set(label.group, [label]);
  }
  const kept = new Set<string>();
  for (const group of groups.values()) {
    const ordered = [...group].sort(
      (a, b) => b.priority - a.priority || a.x - b.x || compareStrings(a.id, b.id),
    );
    const placed: SceneLabel[] = [];
    for (const label of ordered) {
      const halfWidth = (LABEL_CHAR_PX * label.text.length) / 2;
      const collides = placed.some(
        (other) =>
          other.y === label.y &&
          Math.abs(other.x - label.x) <
            halfWidth + (LABEL_CHAR_PX * other.text.length) / 2 + LABEL_PAD_PX,
      );
      if (collides) continue;
      placed.push(label);
      kept.add(label.id);
    }
  }
  return candidates.filter((label) => kept.has(label.id));
}

export function layoutTemporalScene(
  projection: JourneyProjection,
  options: LayoutOptions,
): TemporalSceneModel {
  const { viewport, zoom, branches, selectedLaneId, selectedEventId, reveal, hiddenKinds } =
    options;
  const { left, width } = viewport;
  const axisRight = width - viewport.right;
  const span = axisSpan(viewport);
  const x = (time: number): number => timeToX(viewport, time);
  const clampX = (value: number): number => Math.min(Math.max(value, left), axisRight);
  const inWindow = (value: number): boolean => value >= left && value <= axisRight;

  // --- hierarchy indexes -----------------------------------------------------
  const lanes = projection.lanes;
  const laneById = new Map(lanes.map((lane) => [lane.id, lane] as const));
  const childrenOf = new Map<string, JourneyLane[]>();
  for (const lane of lanes) {
    if (lane.parentId === null || !laneById.has(lane.parentId)) continue;
    const bucket = childrenOf.get(lane.parentId);
    if (bucket) bucket.push(lane);
    else childrenOf.set(lane.parentId, [lane]);
  }
  const descendantCache = new Map<string, readonly JourneyLane[]>();
  const descendantsOf = (id: string): readonly JourneyLane[] => {
    const cached = descendantCache.get(id);
    if (cached) return cached;
    const out: JourneyLane[] = [];
    const seen = new Set<string>([id]);
    const stack = [...(childrenOf.get(id) ?? [])].reverse();
    while (stack.length > 0) {
      const next = stack.pop();
      if (next === undefined || seen.has(next.id)) continue;
      seen.add(next.id);
      out.push(next);
      for (const child of [...(childrenOf.get(next.id) ?? [])].reverse()) stack.push(child);
    }
    descendantCache.set(id, out);
    return out;
  };
  const ancestorsOf = (id: string): string[] => {
    const out: string[] = [];
    const seen = new Set<string>([id]);
    let cursor = laneById.get(id)?.parentId ?? null;
    while (cursor !== null && !seen.has(cursor)) {
      seen.add(cursor);
      out.push(cursor);
      cursor = laneById.get(cursor)?.parentId ?? null;
    }
    return out;
  };

  // --- collapse & visibility -------------------------------------------------
  const denseDefault = lanes.length > options.denseLaneThreshold;
  // A dense page bundles at the deepest hierarchy level whose lanes still fit
  // the threshold: a lone orchestrator opens onto its workstreams, not into
  // one bundle of everything.
  let denseDepth: number | null = null;
  if (denseDefault) {
    denseDepth = 0;
    for (let depth = 1; lanes.filter((lane) => lane.depth <= depth).length <= options.denseLaneThreshold; depth += 1) {
      if (!lanes.some((lane) => lane.depth > depth)) break;
      denseDepth = depth;
    }
  }
  // Workstream zoom bundles where the work first fans out: the shallowest
  // level at which more than one session delegates.
  let workstreamDepth = 0;
  const maxDepth = lanes.reduce((max, lane) => Math.max(max, lane.depth), 0);
  for (let depth = 0; depth <= maxDepth; depth += 1) {
    if (lanes.filter((lane) => lane.depth === depth && (childrenOf.get(lane.id)?.length ?? 0) > 0).length > 1) {
      workstreamDepth = depth;
      break;
    }
  }
  const isCollapsed = (lane: JourneyLane): boolean => {
    if ((childrenOf.get(lane.id)?.length ?? 0) === 0) return false;
    switch (zoom) {
      case 'workstream':
        return lane.depth === workstreamDepth;
      case 'agent':
      case 'event':
        break;
      default: {
        const unhandled: never = zoom;
        return unhandled;
      }
    }
    if (branches.collapsed.has(lane.id)) return true;
    return lane.depth === denseDepth && !branches.expanded.has(lane.id);
  };
  const collapsedIds = new Set(lanes.filter(isCollapsed).map((lane) => lane.id));
  // The bundle a lane is hidden under is its outermost collapsed ancestor.
  const bundleOf = new Map<string, string | null>();
  for (const lane of lanes) {
    let bundle: string | null = null;
    for (const ancestor of ancestorsOf(lane.id)) {
      if (collapsedIds.has(ancestor)) bundle = ancestor;
    }
    bundleOf.set(lane.id, bundle);
  }
  const visible = lanes.filter((lane) => bundleOf.get(lane.id) === null);
  const visibleIds = new Set(visible.map((lane) => lane.id));
  const bundleIds = new Set(visible.filter((lane) => collapsedIds.has(lane.id)).map((l) => l.id));
  /** The scene lane that stands for a projection lane: itself or its bundle. */
  const standInFor = (id: string): string | null =>
    visibleIds.has(id) ? id : (bundleOf.get(id) ?? null);

  // --- focus -----------------------------------------------------------------
  const focusOf = new Map<string, FocusTreatment>();
  if (selectedLaneId === null) {
    for (const lane of lanes) focusOf.set(lane.id, 'neutral');
  } else {
    for (const lane of lanes) focusOf.set(lane.id, 'context');
    if (laneById.has(selectedLaneId)) {
      for (const ancestor of ancestorsOf(selectedLaneId)) focusOf.set(ancestor, 'path');
      focusOf.set(selectedLaneId, 'selected');
    }
  }
  const focusFor = (id: string): FocusTreatment => focusOf.get(id) ?? 'neutral';

  // --- event dispositions (independent of y) ---------------------------------
  const eventsByLane = new Map<string, JourneyEvent[]>();
  for (const event of projection.events) {
    const bucket = eventsByLane.get(event.laneId);
    if (bucket) bucket.push(event);
    else eventsByLane.set(event.laneId, [event]);
  }
  // A selected bundle still expands its own transcript: the reader opened that
  // session, and its subtree staying collapsed is a separate choice.
  const isExpanded = (lane: JourneyLane): boolean =>
    zoom === 'event' && lane.id === selectedLaneId;
  const isWithheld = (event: JourneyEvent): boolean => {
    if (reveal === null) return false;
    if (event.time !== null && reveal.time !== null && event.time > reveal.time) return true;
    // The transcript chain is what the cursor walks, so its own lane is cut by
    // recorded order even for dated turns: the boundary is then exact.
    return (
      event.laneId === reveal.laneId &&
      event.source === 'transcript' &&
      event.sequence > reveal.sequence
    );
  };

  let eventsFiltered = 0;
  let eventsWithheld = 0;
  let eventsFolded = 0;
  let eventsCulled = 0;
  const pendingByLane = new Map<string, PendingNode[]>();
  for (const lane of visible) {
    const bundle = bundleIds.has(lane.id);
    const expanded = isExpanded(lane);
    const pending: PendingNode[] = [];
    for (const event of eventsByLane.get(lane.id) ?? []) {
      if (hiddenKinds.has(event.kind)) {
        eventsFiltered += 1;
        continue;
      }
      if (isWithheld(event)) {
        eventsWithheld += 1;
        continue;
      }
      if (event.source === 'transcript' && !expanded) {
        eventsFolded += 1;
        continue;
      }
      // A bundle's spawn marks all point at children hidden inside it; the
      // cluster's count carries them instead of a fan of curves to nowhere.
      if (bundle && event.kind === 'spawn') {
        eventsFolded += 1;
        continue;
      }
      if (event.time !== null) {
        const px = x(event.time);
        if (!inWindow(px)) {
          eventsCulled += 1;
          continue;
        }
        pending.push({ event, xBasis: 'time', x: px });
      } else {
        pending.push({ event, xBasis: 'sequence', x: Number.NaN });
      }
    }
    pendingByLane.set(lane.id, pending);
  }
  const undatedCount = (laneId: string): number =>
    (pendingByLane.get(laneId) ?? []).filter((node) => node.xBasis === 'sequence').length;

  // --- rows ------------------------------------------------------------------
  // A dated cursor is a wall: no thread, body or span is drawn past it, and a
  // lane whose recorded start lies beyond it is not drawn at all.
  const revealX = reveal !== null && reveal.time !== null ? x(reveal.time) : null;
  const rawExtentOf = (lane: JourneyLane): readonly [number, number] => {
    const x0 = x(lane.start);
    const x1 = lane.end !== null ? x(lane.end) : x0 + OPEN_TAIL_PX;
    return [x0, x1];
  };
  const sceneLanes: SceneLane[] = [];
  const sceneLaneById = new Map<string, SceneLane>();
  let cursorY = RULER_HEIGHT;
  let previousProvider: string | null = null;
  visible.forEach((lane, row) => {
    if (previousProvider !== null && lane.provider !== previousProvider) cursorY += RAIL_GAP_PX;
    previousProvider = lane.provider;
    const bundle = bundleIds.has(lane.id);
    const expanded = isExpanded(lane);
    const height = rowHeight(zoom, expanded, undatedCount(lane.id));
    const y = cursorY + height / 2;
    cursorY += height;

    let [rawX0, rawX1] = rawExtentOf(lane);
    const descendants = bundle ? descendantsOf(lane.id) : [];
    for (const member of descendants) {
      const [memberX0, memberX1] = rawExtentOf(member);
      rawX0 = Math.min(rawX0, memberX0);
      rawX1 = Math.max(rawX1, memberX1);
    }
    const revealed = revealX === null || rawX0 <= revealX;
    if (revealX !== null) rawX1 = Math.min(rawX1, revealX);
    const sceneLane: SceneLane = {
      id: lane.id,
      kind: bundle ? 'bundle' : 'session',
      label: lane.label,
      provider: lane.provider,
      depth: lane.depth,
      y,
      height,
      x0: clampX(rawX0),
      x1: clampX(Math.max(rawX0, rawX1)),
      endSource: lane.endSource,
      focus: focusFor(lane.id),
      expanded,
      offscreen: rawX1 < left || rawX0 > axisRight,
      revealed,
      collapsedDescendants: descendants.length,
      row,
    };
    sceneLanes.push(sceneLane);
    sceneLaneById.set(lane.id, sceneLane);
  });
  const height = cursorY + BOTTOM_PAD_PX;

  // --- rails -----------------------------------------------------------------
  const rails: SceneRail[] = [];
  for (const lane of sceneLanes) {
    const last = rails[rails.length - 1];
    if (last && last.label === lane.provider) {
      rails[rails.length - 1] = {
        ...last,
        y1: lane.y + lane.height / 2,
        lanes: last.lanes + 1,
      };
    } else {
      rails.push({
        id: `rail:${lane.provider}:${lane.row}`,
        kind: 'provider',
        label: lane.provider,
        y0: lane.y - lane.height / 2,
        y1: lane.y + lane.height / 2,
        lanes: 1,
      });
    }
  }

  // --- nodes -----------------------------------------------------------------
  const nodes: SceneNode[] = [];
  const nodeById = new Map<string, SceneNode>();
  const linkedEventIds = new Map<string, string>();
  const gutterYOf = (lane: SceneLane): number =>
    lane.y + lane.height / 2 - SEQUENCE_GUTTER_RISE_PX;
  const undatedByLane = new Map<string, SceneNode[]>();
  for (const sceneLane of sceneLanes) {
    const pending = pendingByLane.get(sceneLane.id);
    if (!pending || pending.length === 0) continue;
    const undated = pending.filter((node) => node.xBasis === 'sequence');
    const gutterX0 = sceneLane.x0;
    const gutterW = Math.max(sceneLane.x1 - sceneLane.x0, SEQUENCE_GUTTER_MIN_PX);
    const gutterY = gutterYOf(sceneLane);
    const placed = pending.map((node) => {
      if (node.xBasis === 'sequence') {
        const k = undated.indexOf(node);
        return {
          ...node,
          x: gutterX0 + ((k + 1) / (undated.length + 1)) * gutterW,
          y: gutterY,
        };
      }
      return { ...node, y: sceneLane.y };
    });
    const halfHits = halfHitsFor(placed);
    const laneUndated: SceneNode[] = [];
    placed.forEach((node, index) => {
      const scene: SceneNode = {
        id: node.event.id,
        laneId: sceneLane.id,
        kind: node.event.kind,
        x: node.x,
        y: node.y,
        xBasis: node.xBasis,
        grade: node.event.grade,
        source: node.event.source,
        label: node.event.label,
        detail: node.event.detail,
        ref: node.event.ref,
        selected: node.event.id === selectedEventId,
        focus: sceneLane.focus,
        halfHit: halfHits[index] ?? HALF_HIT_MAX_PX,
      };
      nodes.push(scene);
      nodeById.set(scene.id, scene);
      if (node.event.linkedEventId !== undefined) linkedEventIds.set(scene.id, node.event.linkedEventId);
      if (scene.xBasis === 'sequence') laneUndated.push(scene);
    });
    if (laneUndated.length > 0) undatedByLane.set(sceneLane.id, laneUndated);
  }

  // --- paths -----------------------------------------------------------------
  const paths: ScenePath[] = [];
  const maxMessages = lanes.reduce((max, lane) => Math.max(max, lane.messages), 0);
  const ceilingLog = Math.log1p(Math.max(maxMessages, 1));
  for (const sceneLane of sceneLanes) {
    if (!sceneLane.revealed) continue;
    const lane = laneById.get(sceneLane.id);
    const messages = lane?.messages ?? 0;
    paths.push({
      id: `lane:${sceneLane.id}`,
      kind: 'lane',
      fromId: sceneLane.id,
      toId: sceneLane.id,
      grade: sceneLane.kind === 'bundle' ? 'exact' : extentGrade(sceneLane.endSource),
      basis: null,
      focus: sceneLane.focus,
      controls: [sceneLane.x0, sceneLane.y, sceneLane.x1, sceneLane.y],
      weight: messages > 0 && ceilingLog > 0 ? Math.log1p(messages) / ceilingLog : 0,
    });
  }

  let relationsDrawn = 0;
  let relationsWithheld = 0;
  for (const relation of projection.relations) {
    const parent = sceneLaneById.get(relation.fromLaneId);
    if (!parent) continue;
    if (reveal !== null && reveal.time !== null && relation.time !== null && relation.time > reveal.time) {
      relationsWithheld += 1;
      continue;
    }
    // A fork bound to a drawn tool-call glyph leaves from that glyph.
    const origin = relation.fromEventId === undefined ? undefined : nodeById.get(relation.fromEventId);
    const px = origin ? origin.x : relation.time === null ? null : x(relation.time);
    if (px === null) continue;
    const targetId = standInFor(relation.toLaneId);
    if (targetId === null || targetId === parent.id) continue;
    const target = sceneLaneById.get(targetId);
    if (!target) continue;
    if (!inWindow(px)) continue;
    const py = origin ? origin.y : parent.y;
    const ty = target.y;
    const k = Math.min(28, Math.max(8, Math.abs(ty - py) * 0.35));
    const childFocus = focusFor(relation.toLaneId);
    paths.push({
      id: relation.id,
      kind: relation.kind,
      fromId: parent.id,
      toId: target.id,
      grade: relation.grade,
      basis: relation.basis,
      focus: childFocus === 'selected' || childFocus === 'path' ? childFocus : parent.focus,
      controls: [px - k, py, px + k * 0.2, py, px - k * 0.2, ty, px + k, ty],
      weight: null,
    });
    relationsDrawn += 1;
  }

  for (const [laneId, undated] of undatedByLane) {
    const focus = focusFor(laneId);
    for (let index = 1; index < undated.length; index += 1) {
      const from = undated[index - 1];
      const to = undated[index];
      if (!from || !to) continue;
      paths.push({
        id: `seq:${from.id}→${to.id}`,
        kind: 'sequence',
        fromId: from.id,
        toId: to.id,
        grade: 'exact',
        basis: 'recorded order',
        focus,
        controls: [from.x, from.y, to.x, to.y],
        weight: null,
      });
    }
  }

  for (const node of nodes) {
    const linkedId = linkedEventIds.get(node.id);
    const call = linkedId === undefined ? undefined : nodeById.get(linkedId);
    if (!call) continue;
    paths.push({
      id: `edit_link:${node.id}→${call.id}`,
      kind: 'edit_link',
      fromId: node.id,
      toId: call.id,
      grade: 'exact',
      basis: 'edit recorded in the same second as the tool call',
      focus: node.focus,
      controls: [node.x, node.y, call.x, call.y],
      weight: null,
    });
  }

  // --- clusters --------------------------------------------------------------
  const clusters: SceneCluster[] = [];
  for (const sceneLane of sceneLanes) {
    if (sceneLane.kind !== 'bundle' || !sceneLane.revealed) continue;
    const members = descendantsOf(sceneLane.id);
    const memberIds = new Set(members.map((member) => member.id));
    const grades: Partial<Record<EvidenceGrade, number>> = {};
    let x0 = Infinity;
    let x1 = -Infinity;
    let subagents = 0;
    let messages = 0;
    let openEnded = 0;
    for (const member of members) {
      tally(grades, extentGrade(member.endSource));
      if (member.isSubagent) subagents += 1;
      messages += member.messages;
      if (member.end === null) openEnded += 1;
      const startX = x(member.start);
      const endX = x(member.end ?? member.start);
      x0 = Math.min(x0, startX);
      x1 = Math.max(x1, endX);
    }
    for (const relation of projection.relations) {
      if (relation.kind === 'spawn' && memberIds.has(relation.toLaneId)) tally(grades, relation.grade);
    }
    let commits = 0;
    for (const event of projection.events) {
      if (event.kind === 'commit' && memberIds.has(event.laneId)) commits += 1;
    }
    const bodyX0 = clampX(Number.isFinite(x0) ? x0 : sceneLane.x0);
    const bodyX1 = clampX(Number.isFinite(x1) ? x1 : sceneLane.x1);
    clusters.push({
      id: `cluster:${sceneLane.id}`,
      laneId: sceneLane.id,
      memberLaneIds: members.map((member) => member.id),
      x0: bodyX0,
      x1: revealX === null ? bodyX1 : Math.max(bodyX0, Math.min(bodyX1, clampX(revealX))),
      y: sceneLane.y,
      height: sceneLane.height,
      counts: { sessions: members.length, subagents, messages, commits, openEnded },
      grades,
      focus: sceneLane.focus,
    });
  }

  // --- intervals -------------------------------------------------------------
  const intervals: SceneInterval[] = [];
  for (const interval of projection.intervals) {
    const sceneLane = sceneLaneById.get(interval.laneId);
    if (!sceneLane || !sceneLane.revealed) continue;
    const start = Math.min(interval.start, interval.end);
    const end = Math.max(interval.start, interval.end);
    if (end < viewport.window.start || start > viewport.window.end) continue;
    if (reveal !== null && reveal.time !== null && start > reveal.time) continue;
    let y: number;
    switch (interval.kind) {
      case 'git_span':
        y = sceneLane.y + Math.min(10, sceneLane.height * 0.3);
        break;
      case 'proximity':
        y = sceneLane.y;
        break;
      default: {
        const unhandled: never = interval.kind;
        return unhandled;
      }
    }
    const intervalX0 = clampX(x(start));
    intervals.push({
      id: interval.id,
      laneId: interval.laneId,
      kind: interval.kind,
      x0: intervalX0,
      x1: revealX === null ? clampX(x(end)) : Math.max(intervalX0, Math.min(clampX(x(end)), clampX(revealX))),
      y,
      label: interval.label,
      grade: interval.grade,
      tone: interval.tone,
      ref: interval.ref,
    });
  }

  // --- gaps ------------------------------------------------------------------
  const gaps: SceneGap[] = projection.gaps.map((gap) => {
    const base = { id: gap.id, laneId: gap.laneId, kind: gap.kind, grade: gap.grade, detail: gap.detail };
    if (gap.laneId === null) return { ...base, x: null, y: null };
    const standIn = standInFor(gap.laneId);
    const sceneLane = standIn === null ? undefined : sceneLaneById.get(standIn);
    if (!sceneLane) return { ...base, x: null, y: null };
    switch (gap.kind) {
      case 'extent_unknown':
        return { ...base, x: sceneLane.x1, y: sceneLane.y };
      case 'parent_outside_page':
      case 'parent_cycle':
      case 'parentage_conflict':
      case 'parentage_unavailable':
      case 'handoff_unavailable':
        return { ...base, x: sceneLane.x0, y: sceneLane.y };
      case 'edit_time_unrecorded':
        return { ...base, x: (sceneLane.x0 + sceneLane.x1) / 2, y: sceneLane.y + Math.min(10, sceneLane.height * 0.3) };
      case 'undated_events':
        return {
          ...base,
          x: sceneLane.x0,
          y: sceneLane.expanded ? gutterYOf(sceneLane) : sceneLane.y,
        };
      default: {
        const unhandled: never = gap.kind;
        return unhandled;
      }
    }
  });

  // --- ticks -----------------------------------------------------------------
  const ticks: SceneTick[] = axisTicks(viewport.window, span).map((tick) => ({
    x: left + tick.x,
    time: tick.time,
    label: tick.label,
  }));

  // --- labels ----------------------------------------------------------------
  const labelCandidates: SceneLabel[] = [];
  for (const sceneLane of sceneLanes) {
    labelCandidates.push({
      id: `label:lane:${sceneLane.id}`,
      text: sceneLane.label,
      x: LABEL_LANE_X,
      y: sceneLane.y,
      anchor: 'start',
      priority: sceneLane.depth === 0 ? 3 : 2,
      group: 'lane',
    });
  }
  for (const node of nodes) {
    const sceneLane = sceneLaneById.get(node.laneId);
    // A spawn's label is the child's name, which the child's own lane row
    // already prints; only a commit carries a name the column does not.
    const marked = node.kind === 'commit';
    if (!sceneLane || (!sceneLane.expanded && !marked)) continue;
    labelCandidates.push({
      id: `label:node:${node.id}`,
      text: node.label,
      x: node.x,
      y: node.y + LABEL_DROP_PX,
      anchor: 'middle',
      priority: node.selected ? 5 : marked ? 3 : 1,
      group: `lane:${node.laneId}`,
    });
  }
  const labels = resolveLabels(labelCandidates);

  // --- minimap ---------------------------------------------------------------
  const full = fittedWindowFor(projection.extent);
  const fullDuration = Math.max(full.end - full.start, Number.EPSILON);
  const minimapX = (time: number): number => ((time - full.start) / fullDuration) * width;
  const binWidth = fullDuration / MINIMAP_BINS;
  const bins: MinimapBin[] = [];
  for (let index = 0; index < MINIMAP_BINS; index += 1) {
    const t0 = full.start + index * binWidth;
    const t1 = index === MINIMAP_BINS - 1 ? full.end : t0 + binWidth;
    const lastBin = index === MINIMAP_BINS - 1;
    let eventCount = 0;
    for (const event of projection.events) {
      if (event.time === null) continue;
      if (event.time >= t0 && (event.time < t1 || (lastBin && event.time <= t1))) eventCount += 1;
    }
    let laneCount = 0;
    for (const lane of lanes) {
      const end = lane.end ?? lane.start;
      if (lane.start <= t1 && end >= t0) laneCount += 1;
    }
    bins.push({ x0: minimapX(t0), x1: minimapX(t1), events: eventCount, lanes: laneCount });
  }
  const rows = sceneLanes.length;
  const minimap: SceneMinimap = {
    bins,
    lanes: sceneLanes.map((sceneLane) => {
      const lane = laneById.get(sceneLane.id);
      const start = lane?.start ?? full.start;
      const end = lane?.end ?? start;
      return {
        id: sceneLane.id,
        y: MINIMAP_LANE_TOP + (sceneLane.row / Math.max(rows - 1, 1)) * MINIMAP_LANE_SPAN,
        x0: minimapX(start),
        x1: minimapX(end),
        endSource: sceneLane.endSource,
      };
    }),
    window: {
      x0: Math.min(Math.max(minimapX(viewport.window.start), 0), width),
      x1: Math.min(Math.max(minimapX(viewport.window.end), 0), width),
    },
    width,
    height: MINIMAP_HEIGHT,
  };

  // --- cursor ----------------------------------------------------------------
  let cursor: SceneCursor | null = null;
  if (reveal !== null) {
    const laneEvents = eventsByLane.get(reveal.laneId) ?? [];
    const active =
      laneEvents.find(
        (event) => event.source === 'transcript' && event.sequence === reveal.sequence,
      ) ??
      laneEvents.find((event) => event.sequence === reveal.sequence) ??
      null;
    if (active !== null) {
      if (reveal.time !== null) {
        cursor = { x: x(reveal.time), laneId: reveal.laneId, xBasis: 'time' };
      } else {
        const node = nodeById.get(active.id);
        if (node) cursor = { x: node.x, laneId: reveal.laneId, xBasis: 'sequence' };
      }
    }
  }

  return {
    viewport,
    zoom,
    height,
    lanes: sceneLanes,
    nodes,
    paths,
    clusters,
    intervals,
    gaps,
    rails,
    ticks,
    labels,
    minimap,
    cursor,
    counts: {
      lanesTotal: lanes.length,
      lanesVisible: sceneLanes.length,
      lanesCollapsed: bundleIds.size,
      eventsTotal: projection.events.length,
      eventsDrawn: nodes.length,
      eventsCulled,
      eventsWithheld,
      eventsFiltered,
      eventsFolded,
      relationsTotal: projection.relations.length,
      relationsDrawn,
      relationsWithheld,
    },
    denseDepth,
  };
}
