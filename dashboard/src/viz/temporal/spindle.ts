/**
 * The recovered Loom spindle, projected onto the live temporal scene.
 *
 * The envelope is the original `fiberPosition` geometry from the archived
 * JourneyField. It changes Y only. Its anchor is the complete loaded extent,
 * so a replay cursor, a pan, or a collapsed branch cannot reshape the loom.
 * Every filament belongs to an actual provider-qualified session. Convergence
 * is a presentation bundle; causal links still require a projected relation.
 */
import { fittedWindowFor, RULER_HEIGHT, timeToX, xToTime } from './layout.ts';
import type {
  EvidenceGrade, FocusTreatment, JourneyEvent, JourneyEventKind, JourneyLane,
  JourneyProjection, RevealBoundary, SceneCursor, SceneFiber, SceneNode, ScenePath,
  ScenePoint, SceneViewport, TemporalSceneModel,
} from './types.ts';

/** The archived Loom's blue/cyan, teal, gold, purple and slate families. */
export const SPINDLE_COLORS = ['#58b8ff', '#43d7ff', '#77dcd8', '#c8b078', '#b094d9', '#8aa2af'] as const;

/** Original loaded-time envelope. Time and home are never viewport fractions. */
export function fiberPosition(time: number, home: number, group: number, extent: readonly number[]): number {
  const u = Math.max(0, Math.min(1, (time - extent[0]!) / Math.max(1, extent[1]! - extent[0]!)));
  const smooth = (n: number): number => {
    const t = Math.max(0, Math.min(1, n));
    return t * t * (3 - 2 * t);
  };
  const opening = smooth(u / .29) * smooth((1 - u) / .26);
  const spread = .008 + .992 * opening;
  return .515 + (home - .515) * spread +
    (.004 * Math.sin(u * 7 + group * 4) + .006 * Math.sin(u * 9 + home * 8)) * opening;
}

interface SpindleOptions {
  readonly reveal: RevealBoundary | null;
  readonly hiddenKinds: ReadonlySet<JourneyEventKind>;
  readonly selectedLaneId?: string | null;
  readonly selectedEventId?: string | null;
}

interface Home {
  readonly home: number;
  readonly group: number;
  readonly familyId: string;
  readonly colorIndex: number;
  readonly spine: boolean;
}

const OPEN_TAIL_PX = 18;
const SAMPLES = 70;

function gradeFor(lane: JourneyLane): EvidenceGrade {
  return lane.endSource === 'session_end' ? 'exact' : lane.endSource === 'last_message' ? 'inferred' : 'unavailable';
}

/** A polyline shared by canvas, SVG hit paths and the miniature. */
export function pointsPath(points: readonly ScenePoint[]): string {
  return points.map((point, index) => `${index ? 'L' : 'M'} ${point.x} ${point.y}`).join(' ');
}

/** Collision is resolved over the whole curved field, including other lanes.
 * Quiet marks remain exact, keyboard-selectable beads rather than vanishing. */
function placeGlyphs(nodes: readonly SceneNode[], dense: boolean): SceneNode[] {
  const cell = 28;
  const occupied = new Map<string, SceneNode[]>();
  const visible = new Set<string>();
  const priority = (node: SceneNode): number => node.selected ? 4
    : node.kind === 'session_start' || node.kind === 'session_end' ? 0
      : node.kind.startsWith('message_') ? 1 : 2;
  const ordered = [...nodes].sort((a, b) => priority(b) - priority(a) || a.x - b.x || a.id.localeCompare(b.id));
  for (const node of ordered) {
    // In the recovered overview, endpoints and ordinary messages are light
    // beads; plates are reserved for the actions along the fibers.
    if (dense && !node.selected && (node.kind === 'session_start' || node.kind === 'session_end' || node.kind.startsWith('message_'))) continue;
    const ix = Math.floor(node.x / cell), iy = Math.floor(node.y / cell);
    let collision = false;
    for (let dx = -1; dx <= 1; dx += 1) for (let dy = -1; dy <= 1; dy += 1) {
      if ((occupied.get(`${ix + dx}:${iy + dy}`) ?? []).some((other) => Math.hypot(other.x - node.x, other.y - node.y) < 25)) collision = true;
    }
    if (collision && !node.selected) continue;
    visible.add(node.id);
    const key = `${ix}:${iy}`;
    const bucket = occupied.get(key) ?? [];
    bucket.push(node); occupied.set(key, bucket);
  }
  const hitCell = 44;
  const hitBuckets = new Map<string, SceneNode[]>();
  for (const node of nodes) {
    const key = `${Math.floor(node.x / hitCell)}:${Math.floor(node.y / hitCell)}`;
    const bucket = hitBuckets.get(key) ?? [];
    bucket.push(node); hitBuckets.set(key, bucket);
  }
  return nodes.map((node) => {
    const ix = Math.floor(node.x / hitCell), iy = Math.floor(node.y / hitCell);
    let nearest = 44;
    for (let dx = -1; dx <= 1; dx += 1) for (let dy = -1; dy <= 1; dy += 1) {
      for (const other of hitBuckets.get(`${ix + dx}:${iy + dy}`) ?? []) {
        if (node.id !== other.id) nearest = Math.min(nearest, Math.hypot(other.x - node.x, other.y - node.y));
      }
    }
    return { ...node, glyphVisible: visible.has(node.id), hitRadius: nearest === 0 ? .25 : Math.min(22, nearest / 2) };
  });
}

export function layoutSpindle(projection: JourneyProjection, base: TemporalSceneModel, options: SpindleOptions): TemporalSceneModel {
  if (projection.extent === null || projection.lanes.length === 0) return base;
  const extent = projection.extent;
  const bounds = [extent.start, extent.end] as const;
  const laneById = new Map(projection.lanes.map((lane) => [lane.id, lane]));
  const sourceLanes = [...laneById.values()];
  const eventById = new Map(projection.events.map((event) => [event.id, event]));
  const ownerOf = new Map(base.lanes.map((lane) => [lane.id, lane.id]));
  for (const cluster of base.clusters) for (const id of cluster.memberLaneIds) {
    if (!ownerOf.has(id)) ownerOf.set(id, cluster.laneId);
  }
  const ancestors = (id: string): string[] => {
    const chain: string[] = [];
    const seen = new Set<string>([id]);
    let parent = laneById.get(id)?.parentId ?? null;
    while (parent !== null && laneById.has(parent) && !seen.has(parent)) {
      seen.add(parent); chain.push(parent); parent = laneById.get(parent)?.parentId ?? null;
    }
    return chain;
  };
  const roots = sourceLanes.filter((lane) => lane.parentId === null || !laneById.has(lane.parentId));
  const rootMembers = new Map<string, number>();
  const branchingRoots = new Set(roots.filter((root) => sourceLanes.filter((lane) => lane.parentId === root.id).length > 1).map((root) => root.id));
  for (const lane of sourceLanes) {
    const chain = ancestors(lane.id);
    const rootId = chain[chain.length - 1] ?? lane.id;
    rootMembers.set(rootId, (rootMembers.get(rootId) ?? 0) + 1);
  }
  // The largest recorded orchestrator supplies the spine even when unrelated
  // roots share the loaded page. Each branching root keeps its child families.
  const spineId = roots.filter((root) => branchingRoots.has(root.id))
    .sort((a, b) => (rootMembers.get(b.id) ?? 0) - (rootMembers.get(a.id) ?? 0))[0]?.id ?? null;
  const families = new Map<string, JourneyLane[]>();
  for (const lane of sourceLanes) {
    if (lane.id === spineId) continue;
    const chain = [lane.id, ...ancestors(lane.id)];
    const rootId = chain[chain.length - 1]!;
    const familyId = branchingRoots.has(rootId) && chain.length > 1 ? chain[chain.length - 2]! : rootId;
    const members = families.get(familyId) ?? [];
    members.push(lane); families.set(familyId, members);
  }
  const homes = new Map<string, Home>();
  const familyEntries = [...families];
  const familyWeight = familyEntries.reduce((sum, [, members]) => sum + Math.sqrt(members.length), 0);
  let bandStart = 0;
  familyEntries.forEach(([familyId, members], groupIndex) => {
    const band = Math.sqrt(members.length) / Math.max(1, familyWeight);
    const group = bandStart + band / 2;
    bandStart += band;
    members.forEach((lane, index) => homes.set(lane.id, {
      familyId, group, colorIndex: groupIndex,
      home: group + ((index + .5) / members.length - .5) * band * .86,
      spine: false,
    }));
  });
  if (spineId !== null) homes.set(spineId, { home: .515, group: .515, familyId: spineId, colorIndex: 0, spine: true });

  const top = RULER_HEIGHT + 28;
  const room = Math.max(340, Math.min(520, (base.viewport.width - base.viewport.left - base.viewport.right) * .58));
  const height = top + room + 32;
  const colorOf = (id: string): string => {
    const home = homes.get(id);
    return home?.spine ? '#b9efff' : SPINDLE_COLORS[(home?.colorIndex ?? 0) % SPINDLE_COLORS.length]!;
  };
  const yAt = (id: string, time: number, fieldTop = top, fieldHeight = room): number => {
    const home = homes.get(id);
    if (!home) return fieldTop + fieldHeight * .515;
    return fieldTop + fieldHeight * (home.spine ? .515 : fiberPosition(time, home.home, home.group, bounds));
  };
  const yAtX = (id: string, x: number): number => yAt(id, xToTime(base.viewport, x));
  const selectedId = options.selectedLaneId ?? base.lanes.find((lane) => lane.focus === 'selected')?.id ?? null;
  const selectedAncestors = new Set(selectedId === null ? [] : ancestors(selectedId));
  const focusOf = (id: string): FocusTreatment => selectedId === null ? 'neutral'
    : id === selectedId ? 'selected' : selectedAncestors.has(id) ? 'path' : 'context';
  const revealTime = options.reveal?.time ?? null;
  const withheld = (event: JourneyEvent): boolean => {
    if (options.reveal === null) return false;
    if (event.time !== null && revealTime !== null && event.time > revealTime) return true;
    return event.laneId === options.reveal.laneId && event.source === 'transcript' && event.sequence > options.reveal.sequence;
  };
  const eventsByLane = new Map<string, JourneyEvent[]>();
  for (const event of projection.events) {
    const bucket = eventsByLane.get(event.laneId) ?? [];
    bucket.push(event); eventsByLane.set(event.laneId, bucket);
  }
  const sample = (id: string, x0: number, x1: number, viewport: SceneViewport, fieldTop = top, fieldHeight = room, extra: readonly number[] = []): ScenePoint[] => {
    const xs = new Set<number>([x0, x1, ...extra.filter((x) => x >= x0 && x <= x1)]);
    for (let step = 1; step < SAMPLES; step += 1) xs.add(x0 + (x1 - x0) * step / SAMPLES);
    return [...xs].sort((a, b) => a - b).map((x) => ({ x, y: yAt(id, xToTime(viewport, x), fieldTop, fieldHeight) }));
  };
  const spanFor = (lane: JourneyLane, viewport: SceneViewport): readonly [number, number] | null => {
    if (revealTime !== null && lane.start > revealTime) return null;
    const start = timeToX(viewport, lane.start);
    const end = Math.min(
      lane.end === null ? start + OPEN_TAIL_PX : timeToX(viewport, lane.end),
      revealTime === null ? Infinity : timeToX(viewport, revealTime),
    );
    const left = viewport.left, right = viewport.width - viewport.right;
    if (end < left || start > right || end < start) return null;
    return [Math.max(left, start), Math.min(right, end)];
  };
  const fibers: SceneFiber[] = [];
  for (const lane of sourceLanes) {
    const span = spanFor(lane, base.viewport);
    const home = homes.get(lane.id);
    if (!span || !home) continue;
    const anchors = (eventsByLane.get(lane.id) ?? []).flatMap((event) => event.time === null ? [] : [timeToX(base.viewport, event.time)]);
    fibers.push({
      id: lane.id, ownerLaneId: ownerOf.get(lane.id) ?? lane.id,
      familyId: home.familyId, color: colorOf(lane.id),
      label: lane.label, provider: lane.provider, grade: gradeFor(lane),
      focus: focusOf(lane.id), endSource: lane.endSource,
      points: sample(lane.id, span[0], span[1], base.viewport, top, room, anchors),
    });
  }
  const fiberById = new Map(fibers.map((fiber) => [fiber.id, fiber]));

  // Labels occupy the family homes, not the pinched endpoints. Crowded labels
  // are omitted only from this margin; every identity remains in the navigator.
  const lanes = base.lanes.map((lane) => {
    const members = [lane.id, ...(base.clusters.find((cluster) => cluster.laneId === lane.id)?.memberLaneIds ?? [])];
    const memberHomes = members.flatMap((id) => homes.has(id) ? [homes.get(id)!.home] : []);
    const home = memberHomes.reduce((sum, value) => sum + value, 0) / Math.max(1, memberHomes.length);
    return { ...lane, y: top + room * home, height: lane.kind === 'bundle' ? 26 : 30 };
  });
  const laneLabels = new Set<string>();
  const labelSlots: number[] = [];
  const labelPriority = (lane: typeof lanes[number]): number => lane.focus === 'selected' ? 5 : lane.kind === 'bundle' ? 3 : lane.depth === 0 ? 2 : 1;
  for (const lane of [...lanes].sort((a, b) => labelPriority(b) - labelPriority(a) || a.y - b.y)) {
    if (labelSlots.some((y) => Math.abs(y - lane.y) < 32)) continue;
    labelSlots.push(lane.y); laneLabels.add(lane.id);
  }

  const baseNodeById = new Map(base.nodes.map((node) => [node.id, node]));
  const pendingNodes: SceneNode[] = [];
  const dispositions = { eventsFiltered: 0, eventsWithheld: 0, eventsFolded: 0, eventsCulled: 0 };
  const selectedUndated = (selectedId === null ? [] : eventsByLane.get(selectedId) ?? [])
    .filter((event) => event.time === null && !options.hiddenKinds.has(event.kind) && !withheld(event));
  for (const event of projection.events) {
    if (options.hiddenKinds.has(event.kind)) { dispositions.eventsFiltered += 1; continue; }
    if (withheld(event)) { dispositions.eventsWithheld += 1; continue; }
    if (!homes.has(event.laneId) || (event.source === 'transcript' && event.laneId !== selectedId)) {
      dispositions.eventsFolded += 1; continue;
    }
    const existing = baseNodeById.get(event.id);
    let x: number;
    if (event.time !== null) {
      x = timeToX(base.viewport, event.time);
      if (x < base.viewport.left || x > base.viewport.width - base.viewport.right) {
        dispositions.eventsCulled += 1; continue;
      }
    } else if (existing) x = existing.x;
    else if (event.laneId === selectedId) {
      const lane = laneById.get(event.laneId)!;
      const span = spanFor(lane, base.viewport);
      if (!span) { dispositions.eventsCulled += 1; continue; }
      const available = Math.min(base.viewport.width - base.viewport.right - span[0], Math.max(120, span[1] - span[0]));
      x = span[0] + (selectedUndated.indexOf(event) + 1) / (selectedUndated.length + 1) * available;
    } else { dispositions.eventsFolded += 1; continue; }
    // An undated record keeps its separate order gutter; curved presentation
    // never upgrades the gutter position into a claimed wall-clock time.
    pendingNodes.push({
      id: event.id, laneId: event.laneId, kind: event.kind, x,
      y: yAtX(event.laneId, x) + (event.time === null ? 24 : 0),
      xBasis: event.time === null ? 'sequence' : 'time', grade: event.grade,
      source: event.source, label: event.label, detail: event.detail, ref: event.ref,
      selected: event.id === options.selectedEventId || existing?.selected === true,
      focus: focusOf(event.laneId), halfHit: existing?.halfHit ?? 12, color: colorOf(event.laneId),
    });
  }
  const nodes = placeGlyphs(pendingNodes, sourceLanes.length > 24);
  const nodeById = new Map(nodes.map((node) => [node.id, node]));
  const paths: ScenePath[] = [];
  for (const path of base.paths) {
    if (path.kind === 'spawn' || path.kind === 'handoff' || path.kind === 'rejoin' || path.kind === 'result') continue;
    if (path.kind === 'lane') {
      const fiber = fiberById.get(path.fromId);
      if (!fiber) continue;
      const first = fiber.points[0]!, last = fiber.points[fiber.points.length - 1]!;
      paths.push({ ...path, controls: [first.x, first.y, last.x, last.y], points: fiber.points, color: fiber.color });
    } else {
      const from = nodeById.get(path.fromId), to = nodeById.get(path.toId);
      if (!from || !to) continue;
      paths.push({ ...path, controls: [from.x, from.y, to.x, to.y], color: colorOf(from.laneId) });
    }
  }
  let relationsWithheld = 0;
  for (const relation of projection.relations) {
    if (!homes.has(relation.fromLaneId) || !homes.has(relation.toLaneId)) continue;
    if (revealTime !== null && relation.time !== null && relation.time > revealTime) { relationsWithheld += 1; continue; }
    const origin = relation.fromEventId ? nodeById.get(relation.fromEventId) : undefined;
    const originEvent = relation.fromEventId ? eventById.get(relation.fromEventId) : undefined;
    if (originEvent && withheld(originEvent)) { relationsWithheld += 1; continue; }
    const time = originEvent?.time ?? relation.time;
    if (time === null || time === undefined) continue;
    if (revealTime !== null && time > revealTime) { relationsWithheld += 1; continue; }
    const px = origin?.x ?? timeToX(base.viewport, time);
    if (px < base.viewport.left || px > base.viewport.width - base.viewport.right) continue;
    const py = origin?.y ?? yAtX(relation.fromLaneId, px), ty = yAtX(relation.toLaneId, px);
    const k = Math.min(28, Math.max(8, Math.abs(ty - py) * .35));
    const x0 = Math.max(base.viewport.left, px - k);
    const x1 = Math.min(base.viewport.width - base.viewport.right, px + k, revealTime === null ? Infinity : timeToX(base.viewport, revealTime));
    paths.push({
      id: relation.id, kind: relation.kind, fromId: relation.fromLaneId, toId: relation.toLaneId,
      grade: relation.grade, basis: relation.basis, focus: focusOf(relation.toLaneId), weight: null,
      color: colorOf(relation.toLaneId),
      controls: [x0, yAtX(relation.fromLaneId, x0), Math.min(x1, px + k * .2), py, Math.max(x0, Math.min(x1, px - k * .2)), ty, x1, yAtX(relation.toLaneId, x1)],
    });
  }
  const intervals = base.intervals.map((interval) => {
    const points = sample(interval.laneId, interval.x0, interval.x1, base.viewport);
    const offset = interval.kind === 'git_span' ? 8 : 0;
    return { ...interval, y: yAtX(interval.laneId, (interval.x0 + interval.x1) / 2) + offset, points: points.map((point) => ({ ...point, y: point.y + offset })) };
  });
  const gaps = base.gaps.map((gap) => {
    if (gap.laneId === null) return gap;
    const fiber = fiberById.get(gap.laneId);
    if (!fiber || fiber.points.length === 0) return { ...gap, x: null, y: null };
    const first = fiber.points[0]!, last = fiber.points[fiber.points.length - 1]!;
    if (gap.kind === 'extent_unknown') return { ...gap, x: last.x, y: last.y };
    if (gap.kind === 'edit_time_unrecorded') {
      const x = (first.x + last.x) / 2;
      return { ...gap, x, y: yAtX(gap.laneId, x) + 8 };
    }
    return { ...gap, x: first.x, y: first.y + (gap.kind === 'undated_events' ? 24 : 0) };
  });
  const labels = base.labels.flatMap((label) => {
    if (label.group === 'lane') {
      const id = label.id.slice('label:lane:'.length);
      const lane = lanes.find((candidate) => candidate.id === id);
      return lane && laneLabels.has(id) ? [{ ...label, y: lane.y }] : [];
    }
    const node = nodeById.get(label.id.slice('label:node:'.length));
    return node?.glyphVisible ? [{ ...label, y: node.y + 18 }] : [];
  });
  const fullViewport: SceneViewport = { width: base.minimap.width, left: 0, right: 0, window: fittedWindowFor(extent) };
  const minimapLanes = sourceLanes.flatMap((lane) => {
    const span = spanFor(lane, fullViewport), home = homes.get(lane.id);
    if (!span || !home) return [];
    const points = sample(lane.id, span[0], span[1], fullViewport, 6, base.minimap.height - 12);
    return [{ id: lane.id, y: points[Math.floor(points.length / 2)]!.y, x0: span[0], x1: span[1], endSource: lane.endSource, points, color: colorOf(lane.id) }];
  });
  const relationCount = paths.filter((path) => path.kind === 'spawn' || path.kind === 'handoff' || path.kind === 'rejoin' || path.kind === 'result').length;
  const cursorEvents = options.reveal === null ? [] : eventsByLane.get(options.reveal.laneId) ?? [];
  const activeRecord = options.reveal === null ? undefined
    : cursorEvents.find((event) => event.source === 'transcript' && event.sequence === options.reveal!.sequence)
      ?? cursorEvents.find((event) => event.sequence === options.reveal!.sequence);
  const activeNode = activeRecord ? nodeById.get(activeRecord.id) : undefined;
  let cursor: SceneCursor | null = base.cursor;
  // A selected collapsed member may expose an order-only transcript even
  // while its owner remains bundled in the flat scene.
  if (cursor === null && activeNode && options.reveal !== null) {
    cursor = { x: activeNode.x, laneId: activeNode.laneId, xBasis: activeNode.xBasis };
  }
  if (cursor !== null) cursor = {
    ...cursor,
    y: activeNode?.y ?? yAtX(cursor.laneId, cursor.x) + (cursor.xBasis === 'sequence' ? 24 : 0),
  };
  return {
    ...base, height, lanes, nodes, paths, intervals, gaps, labels,
    clusters: base.clusters.map((cluster) => ({ ...cluster, y: lanes.find((lane) => lane.id === cluster.laneId)?.y ?? cluster.y })),
    // Provider bands would imply straight rows under curved fibers. Provider
    // identities remain in labels, the caption and the exact navigator.
    rails: [],
    minimap: { ...base.minimap, lanes: minimapLanes },
    spindle: { extent, top, height: room, fibers, labelLaneIds: laneLabels },
    cursor,
    counts: { ...base.counts, ...dispositions, eventsDrawn: nodes.length, relationsDrawn: relationCount, relationsWithheld },
  };
}
