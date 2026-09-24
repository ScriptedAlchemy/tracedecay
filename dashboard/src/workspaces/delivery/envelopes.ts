import type {
  DeliveryAttentionSourceV1,
  DeliveryInboxProjectV1,
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
} from '../../contracts/generated.ts';
import { providerServes, providerStateSentence } from './evidence.ts';
import {
  activeItems,
  changeRadius,
  evidenceLinks,
  headJoin,
  measuredChange,
  uncorrelatedRows,
  unevaluatedItems,
  type EvidenceLink,
} from './rendererModel.ts';
import type { UmbrellaProjection } from './umbrella.ts';

/**
 * Renderer A, the inbox field. Each registered repository is a hairline
 * envelope on a grid ordered by its canonical repository id, so a project's
 * place never depends on what else was admitted. Inside, each tracked head is
 * a station, and the admitted PRs on that head sit beside it, sized by their
 * served line change. Envelopes are containers, never edges: the only lines
 * drawn are served cross-PR evidence, labelled by basis kind.
 */

export interface EnvelopeStation {
  readonly id: string;
  readonly branchRef: string;
  readonly head: string;
  /** The project's own tracked head, as opposed to a PR branch head. */
  readonly tracked: boolean;
  readonly x: number;
  readonly y: number;
}

export interface EnvelopeMark {
  readonly row: DeliveryInboxPullRequestV1;
  readonly x: number;
  readonly y: number;
  /** `null` when the provider served no identity, so no size is known. */
  readonly radius: number | null;
  readonly change: number | null;
  readonly hollow: boolean;
  readonly absences: readonly string[];
  readonly beacons: readonly DeliveryAttentionSourceV1[];
  readonly unevaluated: number;
}

export interface Envelope {
  readonly project: DeliveryInboxProjectV1;
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  /** Admitted rows for this project in the whole inbox, before filters. */
  readonly admitted: number;
  readonly stations: readonly EnvelopeStation[];
  readonly marks: readonly EnvelopeMark[];
  /** Printed inside an envelope that draws no mark. */
  readonly absence: string | null;
}

export interface EnvelopeLink {
  readonly link: EvidenceLink;
  readonly from: { readonly x: number; readonly y: number };
  readonly to: { readonly x: number; readonly y: number };
}

export interface EnvelopeLayout {
  readonly width: number;
  readonly height: number;
  readonly envelopes: readonly Envelope[];
  readonly omitted: { readonly x: number; readonly y: number; readonly width: number; readonly height: number; readonly count: number } | null;
  readonly links: readonly EnvelopeLink[];
}

const PAD = 16;
const GAP = 18;
const HEADER = 50;
const CELL_W = 104;
const CELL_H = 66;
const STATION_W = 34;
const MIN_ENVELOPE_W = 260;
const EMPTY_BODY = 70;
const SLIM_STATION = 30;

function byPullRequestNumber(left: DeliveryInboxPullRequestV1, right: DeliveryInboxPullRequestV1): number {
  const a = Number(left.pull_request.pull_request_id);
  const b = Number(right.pull_request.pull_request_id);
  if (Number.isFinite(a) && Number.isFinite(b) && a !== b) return a - b;
  return left.id.localeCompare(right.id);
}

function stationKey(branchRef: string, head: string): string {
  return `${branchRef}@${head}`;
}

function envelopeAbsence(project: DeliveryInboxProjectV1, admitted: number): string {
  if (!providerServes(project.provider_state)) return providerStateSentence(project.provider_state);
  if (admitted > 0) return `${admitted} admitted · none match the current filters`;
  return 'indexed-head join served zero pull requests';
}

export function layoutEnvelopes(
  inbox: DeliveryInboxV1,
  rows: readonly DeliveryInboxPullRequestV1[],
  projection: UmbrellaProjection,
  viewport: { readonly width: number; readonly height: number },
): EnvelopeLayout {
  const width = Math.max(320, Math.floor(viewport.width));
  const projects = [...inbox.projects].sort(
    (left, right) =>
      left.repository_id.localeCompare(right.repository_id) || left.project_id.localeCompare(right.project_id),
  );
  const cells = projects.length + (inbox.omitted_projects > 0 ? 1 : 0);
  const fit = Math.max(1, Math.floor((width - 2 * PAD + GAP) / (MIN_ENVELOPE_W + GAP)));
  const aspect = width / Math.max(viewport.height, 1);
  const columns = Math.max(1, Math.min(cells, fit, Math.round(Math.sqrt(cells * aspect)) || 1));
  const envelopeW = (width - 2 * PAD - (columns - 1) * GAP) / columns;
  const perRow = Math.max(1, Math.floor((envelopeW - STATION_W - 24) / CELL_W));

  const ceiling = rows.reduce((max, row) => Math.max(max, measuredChange(row) ?? 0), 1);
  const hollow = uncorrelatedRows(projection, rows);

  type Draft = Omit<Envelope, 'x' | 'y' | 'height' | 'stations' | 'marks'> & {
    bodyHeight: number;
    place: (x: number, y: number) => Pick<Envelope, 'stations' | 'marks'>;
  };

  const drafts: Draft[] = projects.map((project) => {
    const admitted = inbox.pull_requests.filter((row) => row.project_id === project.project_id).length;
    const visible = rows.filter((row) => row.project_id === project.project_id).sort(byPullRequestNumber);
    const heads = new Map<string, { branchRef: string; head: string; tracked: boolean; rows: DeliveryInboxPullRequestV1[] }>();
    heads.set(stationKey(project.branch_ref, project.indexed_head_commit_id), {
      branchRef: project.branch_ref,
      head: project.indexed_head_commit_id,
      tracked: true,
      rows: [],
    });
    for (const row of visible) {
      const key = stationKey(row.branch_ref, row.indexed_head_commit_id);
      const entry = heads.get(key) ?? { branchRef: row.branch_ref, head: row.indexed_head_commit_id, tracked: false, rows: [] };
      entry.rows.push(row);
      heads.set(key, entry);
    }
    const stations = [...heads.entries()];
    const blockHeight = (count: number) =>
      count === 0 ? (visible.length === 0 ? EMPTY_BODY : SLIM_STATION) : Math.ceil(count / perRow) * CELL_H;
    const bodyHeight = stations.reduce((sum, [, entry]) => sum + blockHeight(entry.rows.length), 0);
    return {
      project,
      width: envelopeW,
      admitted,
      absence: visible.length === 0 ? envelopeAbsence(project, admitted) : null,
      bodyHeight,
      place: (x, y) => {
        const placedStations: EnvelopeStation[] = [];
        const marks: EnvelopeMark[] = [];
        let cursor = y + HEADER;
        for (const [key, entry] of stations) {
          const stationY = entry.rows.length === 0 ? cursor + 14 : cursor + CELL_H / 2 - 8;
          placedStations.push({ id: key, branchRef: entry.branchRef, head: entry.head, tracked: entry.tracked, x: x + 20, y: stationY });
          entry.rows.forEach((row, index) => {
            const change = measuredChange(row);
            const join = headJoin(row);
            const absences: string[] = [];
            if (hollow.has(row.id)) absences.push('not joined');
            if (join.kind === 'provider_head_moved') absences.push('head moved');
            if (join.kind === 'not_observed') absences.push('head unobserved');
            marks.push({
              row,
              x: x + STATION_W + 30 + (index % perRow) * CELL_W,
              y: stationY + Math.floor(index / perRow) * CELL_H,
              radius: change === null ? null : changeRadius(change, ceiling),
              change,
              hollow: hollow.has(row.id),
              absences,
              beacons: activeItems(row).map((item) => item.source),
              unevaluated: unevaluatedItems(row).length,
            });
          });
          cursor += blockHeight(entry.rows.length);
        }
        return { stations: placedStations, marks };
      },
    };
  });

  const envelopes: Envelope[] = [];
  let y = PAD;
  let omitted: EnvelopeLayout['omitted'] = null;
  for (let start = 0; start < cells; start += columns) {
    const slice = drafts.slice(start, start + columns);
    const rowHeight = Math.max(HEADER + EMPTY_BODY, ...slice.map((draft) => HEADER + draft.bodyHeight + 10));
    for (let offset = 0; offset < columns && start + offset < cells; offset += 1) {
      const x = PAD + offset * (envelopeW + GAP);
      const draft = drafts[start + offset];
      if (draft === undefined) {
        omitted = { x, y, width: envelopeW, height: rowHeight, count: inbox.omitted_projects };
        continue;
      }
      const { place, bodyHeight: _bodyHeight, ...rest } = draft;
      envelopes.push({ ...rest, x, y, height: rowHeight, ...place(x, y) });
    }
    y += rowHeight + GAP;
  }

  const positions = new Map(envelopes.flatMap((envelope) => envelope.marks.map((mark) => [mark.row.id, mark] as const)));
  const links: EnvelopeLink[] = evidenceLinks(projection, new Set(positions.keys())).flatMap((link) => {
    const from = positions.get(link.from);
    const to = positions.get(link.to);
    return from === undefined || to === undefined ? [] : [{ link, from: { x: from.x, y: from.y }, to: { x: to.x, y: to.y } }];
  });

  return { width, height: Math.max(viewport.height, y - GAP + PAD), envelopes, omitted, links };
}

/** A curve between two marks that bows away from the straight line, more for
 * links that repeat the same pair, so parallel bases stay separable. */
export function linkPath(from: { x: number; y: number }, to: { x: number; y: number }, lane: number): string {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const length = Math.hypot(dx, dy) || 1;
  const bow = 18 + lane * 14;
  const cx = mx + (-dy / length) * bow;
  const cy = my + (dx / length) * bow;
  return `M ${from.x} ${from.y} Q ${cx} ${cy} ${to.x} ${to.y}`;
}

export function linkLabelPoint(from: { x: number; y: number }, to: { x: number; y: number }, lane: number): { x: number; y: number } {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const length = Math.hypot(dx, dy) || 1;
  const bow = (18 + lane * 14) / 2;
  return { x: (from.x + to.x) / 2 + (-dy / length) * bow, y: (from.y + to.y) / 2 + (dx / length) * bow };
}
