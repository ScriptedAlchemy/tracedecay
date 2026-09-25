import type {
  DeliveryAttentionSourceV1,
  DeliveryInboxProjectV1,
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
} from '../../contracts/generated.ts';
import { providerServes, providerStateSentence } from './evidence.ts';
import type { DeliveryLocationPatch } from './deliveryLocation.ts';
import {
  evidenceLinks,
  observationMarks,
  uncorrelatedRows,
  type EvidenceLink,
  type ObservationMark,
} from './rendererModel.ts';
import type { UmbrellaProjection } from './umbrella.ts';

/**
 * The Delivery lanes field. X is time, Y is registered repositories in
 * canonical-id order. The inbox serves no opened or merged time, so a PR bar
 * spans its daemon observation window (provider read snapshots and attention
 * observations) and is labelled that way. Semantic zoom (portfolio →
 * repository → pull request) is the existing URL scope: lanes outside the
 * focus compress into density ribbons with exact counts.
 */
export type LaneZoom = 'portfolio' | 'repository' | 'pull_request';

export function laneZoom(project: string | null, pullRequest: string | null): LaneZoom {
  if (pullRequest !== null) return 'pull_request';
  return project === null ? 'portfolio' : 'repository';
}

/** The URL patch a zoom control writes. Pull-request zoom is entered by
 * selecting a bar, so its control writes nothing. */
export function zoomPatch(zoom: LaneZoom, focusProject: string | null): DeliveryLocationPatch {
  switch (zoom) {
    case 'portfolio':
      return { project: null, pullRequest: null };
    case 'repository':
      return { project: focusProject, pullRequest: null };
    case 'pull_request':
      return {};
    default: {
      const unhandled: never = zoom;
      return unhandled;
    }
  }
}

export interface LaneBead {
  readonly x: number;
  readonly kind: 'attention' | 'unevaluated' | 'provider_read';
  readonly source: DeliveryAttentionSourceV1 | null;
  readonly label: string;
  /** 0 for the oldest observation in the loaded page, 1 for the newest. */
  readonly recency: number;
}

export interface LaneTrack {
  readonly id: 'provider_read' | 'attention';
  readonly label: string;
  readonly y: number;
  readonly beads: readonly LaneBead[];
  /** Printed across a hatched band when the track has nothing served. */
  readonly absence: string | null;
}

export interface LaneBar {
  readonly row: DeliveryInboxPullRequestV1;
  readonly y: number;
  readonly x0: number;
  readonly x1: number;
  /** No timestamp at all: parked at the axis origin with the absence printed. */
  readonly undated: boolean;
  readonly hollow: boolean;
  /** Recency of the newest observation, 0..1 within the loaded page. */
  readonly recency: number;
  readonly beads: readonly LaneBead[];
  readonly tracks: readonly LaneTrack[];
}

export interface LaneSummary {
  readonly admitted: number;
  readonly drawn: number;
  readonly active: number;
  readonly stale: number;
  readonly correlated: number;
}

export interface Lane {
  readonly project: DeliveryInboxProjectV1;
  readonly y: number;
  readonly height: number;
  readonly compressed: boolean;
  readonly summary: LaneSummary;
  readonly bins: readonly { readonly x0: number; readonly x1: number; readonly count: number }[];
  readonly bars: readonly LaneBar[];
  readonly absence: string | null;
}

export interface LaneThread {
  readonly link: EvidenceLink;
  readonly from: { readonly x: number; readonly y: number };
  readonly to: { readonly x: number; readonly y: number };
}

export interface LaneLayout {
  readonly width: number;
  readonly height: number;
  readonly x0: number;
  readonly x1: number;
  /** Height of one PR's interactive row band. */
  readonly rowHeight: number;
  readonly span: { readonly start: number; readonly end: number } | null;
  readonly ticks: readonly { readonly x: number; readonly at: number }[];
  readonly lanes: readonly Lane[];
  readonly threads: readonly LaneThread[];
  /** Served links whose other end sits in a compressed or undrawn lane. */
  readonly hiddenLinks: number;
  readonly omitted: { readonly y: number; readonly height: number; readonly count: number } | null;
}

export const LANE_GUTTER = 196;
/** Room right of the axis so the newest bar's label and beacon codes print. */
const LABEL_ROOM = 104;
const RULER = 30;
const LANE_PAD = 8;
const RIBBON = 34;
const TRACK = 20;
/** Every PR row is a full-width interactive band at the 44px target floor;
 * the drawn bar inside it stays thin. */
export const ROW_HEIGHT = 44;
/** Portfolio lanes with more PRs than this compress into a ribbon. */
export const DENSE_LANE_LIMIT = 8;
const BINS = 32;
/** Ruler labels are `MM-DD HH:MM`; closer than this they overprint. */
const MIN_TICK_GAP = 96;

export function layoutLanes(
  inbox: DeliveryInboxV1,
  rows: readonly DeliveryInboxPullRequestV1[],
  projection: UmbrellaProjection,
  viewport: { readonly width: number; readonly height: number },
  focus: { readonly zoom: LaneZoom; readonly project: string | null; readonly pullRequest: string | null },
): LaneLayout {
  const width = Math.max(420, Math.floor(viewport.width));
  const x0 = LANE_GUTTER + 12;
  const x1 = width - LABEL_ROOM;
  const projects = [...inbox.projects].sort(
    (left, right) =>
      left.repository_id.localeCompare(right.repository_id) || left.project_id.localeCompare(right.project_id),
  );
  const focusRow = rows.find((row) => row.id === focus.pullRequest) ?? null;
  const focusProject = focus.zoom === 'pull_request' ? (focusRow?.project_id ?? focus.project) : focus.project;

  const times = inbox.pull_requests.flatMap((row) => observationMarks(row).map((mark) => mark.at));
  const span = times.length === 0 ? null : { start: Math.min(...times), end: Math.max(...times) };
  const xOf = (at: number) =>
    span === null || span.end === span.start ? (x0 + x1) / 2 : x0 + ((at - span.start) / (span.end - span.start)) * (x1 - x0);
  const recencyOf = (at: number) => (span === null || span.end === span.start ? 1 : (at - span.start) / (span.end - span.start));
  const bead = (mark: ObservationMark): LaneBead => ({
    x: xOf(mark.at),
    kind: mark.attention === null ? 'provider_read' : mark.attention.state === 'active' ? 'attention' : 'unevaluated',
    source: mark.attention?.source ?? null,
    label: mark.label,
    recency: recencyOf(mark.at),
  });

  const expandedFor = (project: DeliveryInboxProjectV1, count: number): boolean =>
    focus.zoom === 'portfolio' ? count <= DENSE_LANE_LIMIT : project.project_id === focusProject;

  const hollow = uncorrelatedRows(projection, inbox.pull_requests);
  const grouped = new Set(projection.umbrellas.flatMap((umbrella) => umbrella.members.map((member) => member.id)));

  let y = RULER;
  const lanes: Lane[] = projects.map((project) => {
    const admittedRows = inbox.pull_requests.filter((row) => row.project_id === project.project_id);
    const visible = rows.filter((row) => row.project_id === project.project_id);
    const expanded = expandedFor(project, visible.length);
    const summary: LaneSummary = {
      admitted: admittedRows.length,
      drawn: expanded ? visible.length : 0,
      active: admittedRows.reduce((sum, row) => sum + row.attention.filter((item) => item.state === 'active').length, 0),
      stale: admittedRows.filter((row) => row.state !== 'current').length,
      correlated: admittedRows.filter((row) => grouped.has(row.id)).length,
    };
    const absence =
      admittedRows.length === 0
        ? providerServes(project.provider_state)
          ? 'indexed-head join served zero pull requests'
          : providerStateSentence(project.provider_state)
        : expanded && visible.length === 0
          ? `${admittedRows.length} admitted · none in the current scope`
          : null;
    const top = y;
    if (!expanded || visible.length === 0) {
      const counts = new Array<number>(BINS).fill(0);
      for (const row of admittedRows) {
        for (const mark of observationMarks(row)) {
          const index = Math.max(0, Math.min(BINS - 1, Math.floor(((xOf(mark.at) - x0) / Math.max(x1 - x0, 1)) * BINS)));
          counts[index] = (counts[index] ?? 0) + 1;
        }
      }
      const binWidth = (x1 - x0) / BINS;
      y += RIBBON;
      return {
        project,
        y: top,
        height: RIBBON,
        compressed: absence === null,
        summary,
        bins: counts.map((count, index) => ({ x0: x0 + index * binWidth, x1: x0 + (index + 1) * binWidth, count })),
        bars: [],
        absence,
      };
    }
    let cursor = top + LANE_PAD;
    const bars: LaneBar[] = [...visible]
      .sort((left, right) => left.id.localeCompare(right.id))
      .map((row) => {
        const marks = observationMarks(row);
        const barY = cursor + ROW_HEIGHT / 2;
        cursor += ROW_HEIGHT;
        const tracks: LaneTrack[] = [];
        if (row.id === focusRow?.id && focus.zoom === 'pull_request') {
          const reads = marks.filter((mark) => mark.kind === 'provider_read').map(bead);
          tracks.push({
            id: 'provider_read',
            label: 'provider reads · observed',
            y: cursor + TRACK / 2,
            beads: reads,
            absence: reads.length === 0 ? 'no provider read snapshot served' : null,
          });
          cursor += TRACK;
          tracks.push({
            id: 'attention',
            label: 'attention · observed',
            y: cursor + TRACK / 2,
            beads: marks.filter((mark) => mark.kind === 'attention').map(bead),
            absence: row.attention.length === 0 ? 'no attention served' : null,
          });
          cursor += TRACK;
        }
        const start = marks[0]?.at;
        const end = marks[marks.length - 1]?.at;
        return {
          row,
          y: barY,
          x0: start === undefined ? x0 : xOf(start),
          x1: end === undefined ? x0 + 6 : xOf(end),
          undated: start === undefined,
          hollow: hollow.has(row.id),
          recency: end === undefined ? 0 : recencyOf(end),
          beads: marks.map(bead),
          tracks,
        };
      });
    y = cursor + LANE_PAD;
    return { project, y: top, height: y - top, compressed: false, summary, bins: [], bars, absence: null };
  });

  const omitted = inbox.omitted_projects > 0 ? { y, height: RIBBON, count: inbox.omitted_projects } : null;
  if (omitted !== null) y += RIBBON;

  const positions = new Map(lanes.flatMap((lane) => lane.bars.map((bar) => [bar.row.id, bar] as const)));
  const threads: LaneThread[] = [];
  let hiddenLinks = 0;
  for (const link of evidenceLinks(projection, new Set(inbox.pull_requests.map((row) => row.id)))) {
    const from = positions.get(link.from);
    const to = positions.get(link.to);
    if (from === undefined || to === undefined) {
      if (from !== undefined || to !== undefined) hiddenLinks += 1;
      continue;
    }
    threads.push({ link, from: { x: (from.x0 + from.x1) / 2, y: from.y }, to: { x: (to.x0 + to.x1) / 2, y: to.y } });
  }

  const tickCount = Math.max(2, Math.min(5, Math.floor((x1 - x0) / MIN_TICK_GAP) + 1));
  const ticks =
    span === null
      ? []
      : Array.from({ length: tickCount }, (_, index) => {
          const at = span.start + ((span.end - span.start) * index) / (tickCount - 1);
          return { x: xOf(at), at };
        });

  return { width, height: Math.max(viewport.height, y + 8), x0, x1, rowHeight: ROW_HEIGHT, span, ticks, lanes, threads, hiddenLinks, omitted };
}

/** A thread between two bars: a cubic that leaves and enters horizontally. */
export function threadPath(from: { x: number; y: number }, to: { x: number; y: number }): string {
  const dy = to.y - from.y;
  return `M ${from.x} ${from.y} C ${from.x + 24} ${from.y + dy * 0.35} ${to.x - 24} ${to.y - dy * 0.35} ${to.x} ${to.y}`;
}
