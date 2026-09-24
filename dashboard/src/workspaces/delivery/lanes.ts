import type {
  DeliveryAttentionSourceV1,
  DeliveryInboxProjectV1,
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
} from '../../contracts/generated.ts';
import { providerServes, providerStateSentence, type EvidenceGrade } from './evidence.ts';
import { laneServes, type JourneyLaneId, type JourneyModel } from './journey.ts';
import { laneStateDetail } from './ProjectionLedger.tsx';
import {
  evidenceLinks,
  observationMarks,
  uncorrelatedRows,
  type EvidenceLink,
} from './rendererModel.ts';
import type { UmbrellaProjection } from './umbrella.ts';

/**
 * Renderer C, the dense delivery field. X is time, Y is registered
 * repositories in canonical-id order. The inbox serves no opened or merged
 * time, so a PR bar spans its daemon observation window and is labelled that
 * way; event time appears only on the focused PR's journey tracks. Semantic
 * zoom (portfolio → repository → pull request) is the existing URL scope:
 * lanes outside the focus compress into density ribbons with exact counts.
 */
export type LaneZoom = 'portfolio' | 'repository' | 'pull_request';

export function laneZoom(project: string | null, pullRequest: string | null): LaneZoom {
  if (pullRequest !== null) return 'pull_request';
  return project === null ? 'portfolio' : 'repository';
}

export interface LaneBead {
  readonly x: number;
  readonly kind: 'attention' | 'unevaluated' | 'provider_read' | 'event' | 'observed';
  readonly source: DeliveryAttentionSourceV1 | null;
  readonly label: string;
  readonly grade: EvidenceGrade | null;
  readonly episodeId: string | null;
}

export interface LaneTrack {
  readonly id: string;
  readonly label: string;
  readonly y: number;
  readonly beads: readonly LaneBead[];
  /** Printed across a hatched band when the track's authority did not serve. */
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
  readonly span: { readonly start: number; readonly end: number } | null;
  readonly ticks: readonly { readonly x: number; readonly at: number }[];
  readonly lanes: readonly Lane[];
  readonly threads: readonly LaneThread[];
  /** Served links whose other end sits in a compressed lane. */
  readonly hiddenLinks: number;
  readonly omitted: { readonly y: number; readonly height: number; readonly count: number } | null;
}

export const LANE_GUTTER = 196;
/** Room right of the axis so the newest bar's label and beacon codes print. */
const LABEL_ROOM = 104;
const RULER = 30;
const LANE_PAD = 10;
const RIBBON = 34;
const TRACK = 20;
const MIN_ROW = 22;
const MAX_ROW = 44;
/** Portfolio lanes with more PRs than this compress into a ribbon. */
export const DENSE_LANE_LIMIT = 8;
const BINS = 32;
/** Ruler labels are `MM-DD HH:MM`; closer than this they overprint. */
const MIN_TICK_GAP = 96;

const FOCUS_TRACKS: readonly (readonly [JourneyLaneId, string])[] = [
  ['commits', 'commits · event'],
  ['reviews', 'reviews · observed'],
  ['checks', 'checks · observed'],
];

export function layoutLanes(
  inbox: DeliveryInboxV1,
  rows: readonly DeliveryInboxPullRequestV1[],
  projection: UmbrellaProjection,
  viewport: { readonly width: number; readonly height: number },
  focus: { readonly zoom: LaneZoom; readonly project: string | null; readonly pullRequest: string | null; readonly journey: JourneyModel | null },
): LaneLayout {
  const width = Math.max(420, Math.floor(viewport.width));
  const x0 = LANE_GUTTER + 12;
  const x1 = width - LABEL_ROOM;
  const projects = [...inbox.projects].sort(
    (left, right) =>
      left.repository_id.localeCompare(right.repository_id) || left.project_id.localeCompare(right.project_id),
  );
  const focusRow = rows.find((row) => row.id === focus.pullRequest) ?? inbox.pull_requests.find((row) => row.id === focus.pullRequest) ?? null;
  const focusProject = focus.zoom === 'pull_request' ? (focusRow?.project_id ?? focus.project) : focus.project;

  const times = rows.flatMap((row) => observationMarks(row).map((mark) => mark.at));
  const episodeTimes = focus.journey?.episodes.map((episode) => episode.at).filter((at): at is number => at !== null) ?? [];
  const all = [...times, ...episodeTimes];
  const span = all.length === 0 ? null : { start: Math.min(...all), end: Math.max(...all) };
  const xOf = (at: number) =>
    span === null || span.end === span.start ? (x0 + x1) / 2 : x0 + ((at - span.start) / (span.end - span.start)) * (x1 - x0);

  const expandedFor = (project: DeliveryInboxProjectV1, count: number): boolean => {
    if (focus.zoom === 'portfolio') return count <= DENSE_LANE_LIMIT;
    return project.project_id === focusProject;
  };
  const drawnRows = projects.flatMap((project) => {
    const visible = rows.filter((row) => row.project_id === project.project_id);
    return expandedFor(project, visible.length) ? visible : [];
  });
  const trackCount = focus.zoom === 'pull_request' && focusRow !== null ? 2 + (focus.journey === null ? 0 : FOCUS_TRACKS.length) : 0;
  const expandedLanes = projects.filter((project) =>
    expandedFor(project, rows.filter((row) => row.project_id === project.project_id).length),
  ).length;
  const budget = viewport.height - RULER - (projects.length - expandedLanes) * RIBBON - expandedLanes * 2 * LANE_PAD - trackCount * TRACK;
  const rowHeight = Math.max(MIN_ROW, Math.min(MAX_ROW, budget / Math.max(drawnRows.length, 1)));

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
      admittedRows.length > 0
        ? null
        : providerServes(project.provider_state)
          ? 'indexed-head join served zero pull requests'
          : providerStateSentence(project.provider_state);
    const top = y;
    if (!expanded || absence !== null || visible.length === 0) {
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
        absence: absence ?? (visible.length === 0 && expanded ? `${admittedRows.length} admitted · none match the current filters` : null),
      };
    }
    let cursor = top + LANE_PAD;
    const bars: LaneBar[] = [...visible]
      .sort((left, right) => left.id.localeCompare(right.id))
      .map((row) => {
        const marks = observationMarks(row);
        const barY = cursor + rowHeight / 2;
        cursor += rowHeight;
        const focused = row.id === focusRow?.id && focus.zoom === 'pull_request';
        const tracks: LaneTrack[] = [];
        if (focused) {
          const trackBeads = (kind: 'provider_read' | 'attention'): LaneBead[] =>
            marks
              .filter((mark) => mark.kind === kind)
              .map((mark) => ({
                x: xOf(mark.at),
                kind: mark.attention === null ? 'provider_read' : mark.attention.state === 'active' ? 'attention' : 'unevaluated',
                source: mark.attention?.source ?? null,
                label: mark.label,
                grade: null,
                episodeId: null,
              }));
          tracks.push({ id: 'provider_read', label: 'provider reads · observed', y: cursor + TRACK / 2, beads: trackBeads('provider_read'), absence: null });
          cursor += TRACK;
          const attention = trackBeads('attention');
          tracks.push({
            id: 'attention',
            label: 'attention · observed',
            y: cursor + TRACK / 2,
            beads: attention,
            absence: row.attention.length === 0 ? 'no attention served' : null,
          });
          cursor += TRACK;
          if (focus.journey !== null) {
            for (const [laneId, label] of FOCUS_TRACKS) {
              const lane = focus.journey.lanes.find((candidate) => candidate.id === laneId);
              if (lane === undefined) continue;
              tracks.push({
                id: laneId,
                label,
                y: cursor + TRACK / 2,
                beads: lane.episodes
                  .filter((episode) => episode.at !== null)
                  .map((episode) => ({
                    x: xOf(episode.at as number),
                    kind: episode.timeKind === 'event' ? ('event' as const) : ('observed' as const),
                    source: null,
                    label: episode.label,
                    grade: episode.grade,
                    episodeId: episode.id,
                  })),
                absence: laneServes(lane.state)
                  ? lane.episodes.length === 0
                    ? 'served empty'
                    : null
                  : `${lane.state.kind.replaceAll('_', ' ')} · ${laneStateDetail(lane.state) ?? ''}`,
              });
              cursor += TRACK;
            }
          }
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
          beads: marks.map((mark) => ({
            x: xOf(mark.at),
            kind: mark.attention === null ? 'provider_read' : mark.attention.state === 'active' ? 'attention' : 'unevaluated',
            source: mark.attention?.source ?? null,
            label: mark.label,
            grade: null,
            episodeId: null,
          })),
          tracks,
        };
      });
    y = cursor + LANE_PAD;
    return { project, y: top, height: y - top, compressed: false, summary, bins: [], bars, absence: null };
  });

  const omitted = inbox.omitted_projects > 0 ? { y, height: RIBBON, count: inbox.omitted_projects } : null;
  if (omitted !== null) y += RIBBON;

  const positions = new Map(lanes.flatMap((lane) => lane.bars.map((bar) => [bar.row.id, bar] as const)));
  const allLinks = evidenceLinks(projection, new Set(inbox.pull_requests.map((row) => row.id)));
  const threads: LaneThread[] = [];
  let hiddenLinks = 0;
  for (const link of allLinks) {
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

  return { width, height: Math.max(viewport.height, y + 8), x0, x1, span, ticks, lanes, threads, hiddenLinks, omitted };
}

/** A thread between two bars: a cubic that leaves and enters horizontally. */
export function threadPath(from: { x: number; y: number }, to: { x: number; y: number }): string {
  const dy = to.y - from.y;
  return `M ${from.x} ${from.y} C ${from.x + 24} ${from.y + dy * 0.35} ${to.x - 24} ${to.y - dy * 0.35} ${to.x} ${to.y}`;
}
