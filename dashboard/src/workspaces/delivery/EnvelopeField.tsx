import { useMemo, useState, type KeyboardEvent } from 'react';
import type { DeliveryInboxPullRequestV1, DeliveryInboxV1 } from '../../contracts/generated.ts';
import { Corners } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { gradeDash, gradeLabel } from './evidence.ts';
import { layoutEnvelopes, linkLabelPoint, linkPath, type EnvelopeMark } from './envelopes.ts';
import {
  AttentionBeacon,
  AttentionLegend,
  GradeLegend,
  HatchDefs,
  UnevaluatedGlyph,
  useMeasuredSize,
} from './rendererMarks.tsx';
import { attentionCode, headJoin, headJoinSentence, UNCORRELATED_SENTENCE } from './rendererModel.ts';
import type { UmbrellaProjection } from './umbrella.ts';

const HIT = 44;

function shortRef(ref: string): string {
  return ref.replace(/^refs\/heads\//, '');
}

/** Mono text clipped to a pixel budget with an ellipsis; the full value is in
 * the mark's accessible label and the exact table. */
function fit(text: string, pixels: number, charWidth = 5.2): string {
  const limit = Math.max(4, Math.floor(pixels / charWidth));
  return text.length <= limit ? text : `${text.slice(0, limit - 1)}…`;
}

/** Keeps the head suffix whole and clips the branch name before it. */
function fitRef(name: string, suffix: string, pixels: number): string {
  return `${fit(name, pixels - suffix.length * 5.2)}${suffix}`;
}

function markLabel(mark: EnvelopeMark): string {
  const row = mark.row;
  const title = row.pull_request.identity?.title ?? row.pull_request.label;
  const size = mark.change === null ? 'size not served' : `${mark.change.toLocaleString('en-US')} lines changed`;
  const attention =
    mark.beacons.length === 0 ? 'no active attention' : `active attention ${mark.beacons.map(attentionCode).join(', ')}`;
  const absences = [mark.hollow ? UNCORRELATED_SENTENCE : null, headJoinSentence(headJoin(row))].filter(Boolean);
  return `Pull request #${row.pull_request.pull_request_id} · ${title} · ${size} · ${attention} · ${row.state} · ${absences.join(' · ')}`;
}

/**
 * Renderer A: registered repositories as hairline envelopes, tracked heads as
 * stations, admitted PRs as marks sized by served change. Hover inspects,
 * focus is a 2px cyan frame, selection writes the same URL the list does.
 */
export function EnvelopeField({
  inbox,
  rows,
  projection,
  selectedRowId,
  onSelectRow,
}: {
  inbox: DeliveryInboxV1;
  rows: readonly DeliveryInboxPullRequestV1[];
  projection: UmbrellaProjection;
  selectedRowId: string | null;
  onSelectRow: (row: DeliveryInboxPullRequestV1) => void;
}) {
  const [ref, size] = useMeasuredSize({ width: 720, height: 520 });
  const layout = useMemo(() => layoutEnvelopes(inbox, rows, projection, size), [inbox, rows, projection, size]);
  const [hovered, setHovered] = useState<string | null>(null);
  const [focused, setFocused] = useState<string | null>(null);
  const anchor = hovered ?? selectedRowId;
  const neighbors = useMemo(() => {
    if (anchor === null) return null;
    const set = new Set([anchor]);
    for (const { link } of layout.links) {
      if (link.from === anchor) set.add(link.to);
      if (link.to === anchor) set.add(link.from);
    }
    return set;
  }, [anchor, layout.links]);
  const marks = layout.envelopes.flatMap((envelope) => envelope.marks);
  const hoveredMark = marks.find((mark) => mark.row.id === hovered) ?? null;
  const pairLanes = new Map<string, number>();
  const activate = (event: KeyboardEvent<SVGGElement>, row: DeliveryInboxPullRequestV1) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      onSelectRow(row);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div
        ref={ref}
        className="td-optic td-grain relative min-h-72 flex-1 overflow-auto"
        data-delivery-renderer="envelopes"
      >
        <Corners tone="signal" />
        <svg
          width={layout.width}
          height={layout.height}
          className="relative z-[1] block"
          role="group"
          aria-label="Delivery inbox field · repositories as envelopes"
        >
          <defs>
            <HatchDefs id="envelope-hatch" />
          </defs>

          {layout.envelopes.map((envelope) => (
            <g key={envelope.project.project_id} data-envelope={envelope.project.project_id}>
              <rect
                x={envelope.x}
                y={envelope.y}
                width={envelope.width}
                height={envelope.height}
                fill="var(--raw-graph-substrate)"
                fillOpacity="0.55"
                stroke="var(--raw-graph-edge)"
                strokeWidth="1"
              />
              <text x={envelope.x + 12} y={envelope.y + 17} fontSize="11" fontFamily="var(--font-mono)" letterSpacing="0.12em" fill="var(--raw-graph-text)">
                {fit(envelope.project.label.toUpperCase(), envelope.width - 24, 7.4)}
              </text>
              <text x={envelope.x + 12} y={envelope.y + 30} fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.7">
                {fit(`${envelope.marks.length}/${envelope.admitted} PRs · provider ${envelope.project.provider_state.replaceAll('_', ' ')}`, envelope.width - 24)}
              </text>
              <text x={envelope.x + 12} y={envelope.y + 41} fontSize="8" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.45">
                {fit(envelope.project.repository_id, envelope.width - 24, 4.9)}
              </text>
              {envelope.absence === null ? null : (
                <g>
                  <rect
                    x={envelope.x + 12}
                    y={envelope.y + 76}
                    width={envelope.width - 24}
                    height={envelope.height - 86}
                    fill="url(#envelope-hatch)"
                    stroke="var(--raw-graph-edge)"
                    strokeDasharray="1 5"
                  />
                  <text x={envelope.x + 20} y={envelope.y + 92} fontSize="9" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                    NO PULL REQUEST DRAWN
                  </text>
                  <text x={envelope.x + 20} y={envelope.y + 105} fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.75">
                    {fit(envelope.absence, envelope.width - 40)}
                  </text>
                </g>
              )}
              {envelope.stations.map((station) => {
                const onStation = envelope.marks.filter((mark) => mark.row.branch_ref === station.branchRef && mark.row.indexed_head_commit_id === station.head);
                const last = onStation[onStation.length - 1];
                return (
                  <g key={station.id} aria-hidden>
                    {onStation.length > 0 && last !== undefined ? (
                      <line x1={station.x} y1={station.y} x2={Math.max(...onStation.map((mark) => mark.x))} y2={station.y} stroke="var(--raw-graph-edge)" strokeWidth="1" />
                    ) : null}
                    <rect
                      x={station.x - 5}
                      y={station.y - 5}
                      width="10"
                      height="10"
                      fill={station.tracked ? 'var(--raw-graph-substrate)' : 'none'}
                      stroke="var(--raw-graph-accent)"
                      strokeWidth="1.2"
                    />
                    <text x={station.x + 12} y={station.y - (onStation.length > 0 ? 22 : -3)} fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.75">
                      {fitRef(`${station.tracked ? 'tracked ' : ''}${shortRef(station.branchRef)}`, ` @ ${station.head.slice(0, 7)}`, envelope.x + envelope.width - station.x - 22)}
                    </text>
                  </g>
                );
              })}
            </g>
          ))}

          {layout.omitted === null ? null : (
            <g data-envelope="omitted">
              <rect
                x={layout.omitted.x}
                y={layout.omitted.y}
                width={layout.omitted.width}
                height={layout.omitted.height}
                fill="url(#envelope-hatch)"
                stroke="var(--raw-graph-edge)"
                strokeDasharray="4 3"
              />
              <text x={layout.omitted.x + 12} y={layout.omitted.y + 17} fontSize="11" fontFamily="var(--font-mono)" letterSpacing="0.12em" fill="var(--raw-graph-text)">
                {`${layout.omitted.count} REGISTERED PROJECT${layout.omitted.count === 1 ? '' : 'S'} OMITTED`}
              </text>
              <text x={layout.omitted.x + 12} y={layout.omitted.y + 32} fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.7">
                no indexed head yet · no pull request can be joined
              </text>
            </g>
          )}

          <g aria-hidden>
            {layout.links.map(({ link, from, to }) => {
              const pair = [link.from, link.to].sort().join('|');
              const lane = pairLanes.get(pair) ?? 0;
              pairLanes.set(pair, lane + 1);
              const lit = neighbors === null || (neighbors.has(link.from) && neighbors.has(link.to) && (link.from === anchor || link.to === anchor));
              const label = linkLabelPoint(from, to, lane);
              return (
                <g key={link.id} opacity={lit ? 1 : 0.22} data-link={link.kind}>
                  <path
                    d={linkPath(from, to, lane)}
                    fill="none"
                    stroke="var(--raw-graph-text)"
                    strokeOpacity="0.75"
                    strokeWidth="1.1"
                    strokeDasharray={gradeDash(link.grade)}
                  />
                  <rect x={label.x - 3} y={label.y - 8} width={link.code.length * 5.4 + 6} height="11" fill="var(--raw-graph-substrate)" />
                  <text x={label.x} y={label.y} fontSize="8" fontFamily="var(--font-mono)" letterSpacing="0.08em" fill="var(--raw-graph-text)">
                    {link.code}
                  </text>
                </g>
              );
            })}
          </g>

          {marks.map((mark) => {
            const selected = mark.row.id === selectedRowId;
            const dim = neighbors !== null && !neighbors.has(mark.row.id);
            const r = mark.radius ?? 5;
            return (
              <g
                key={mark.row.id}
                role="button"
                tabIndex={0}
                data-delivery-mark={mark.row.id}
                aria-label={markLabel(mark)}
                aria-pressed={selected}
                className="cursor-pointer outline-none transition-opacity motion-reduce:transition-none"
                opacity={dim ? 0.35 : 1}
                onClick={() => onSelectRow(mark.row)}
                onKeyDown={(event) => activate(event, mark.row)}
                onMouseEnter={() => setHovered(mark.row.id)}
                onMouseLeave={() => setHovered(null)}
                onFocus={() => setFocused(mark.row.id)}
                onBlur={() => setFocused(null)}
              >
                <rect x={mark.x - HIT / 2} y={mark.y - HIT / 2} width={HIT} height={HIT} fill="transparent" />
                {focused === mark.row.id ? (
                  <rect x={mark.x - r - 5} y={mark.y - r - 5} width={2 * r + 10} height={2 * r + 10} fill="none" stroke="var(--raw-graph-accent)" strokeWidth="2" />
                ) : null}
                {mark.radius === null ? (
                  <rect x={mark.x - 5} y={mark.y - 5} width="10" height="10" fill="none" stroke="var(--raw-graph-accent)" strokeDasharray="2 2" />
                ) : (
                  <circle
                    cx={mark.x}
                    cy={mark.y}
                    r={r}
                    fill={selected ? 'var(--raw-graph-accent)' : mark.hollow ? 'none' : 'color-mix(in oklab, var(--raw-graph-accent) 22%, var(--raw-graph-substrate))'}
                    stroke="var(--raw-graph-accent)"
                    strokeWidth={selected ? 2 : 1.2}
                  />
                )}
                {mark.row.state === 'current' ? null : (
                  <circle cx={mark.x} cy={mark.y} r={r + 4} fill="none" stroke="var(--raw-graph-text)" strokeOpacity="0.6" strokeDasharray={mark.row.state === 'stale' ? '2 2' : '4 3'} />
                )}
                {selected ? <rect x={mark.x - r} y={mark.y + r + 6} width={2 * r} height="2" fill="var(--raw-graph-accent)" /> : null}
                {mark.beacons.map((source, index) => (
                  <AttentionBeacon key={`${source}:${index}`} x={mark.x + r + 8} y={mark.y - r + 3 + index * 11} source={source} />
                ))}
                {mark.unevaluated > 0 ? <UnevaluatedGlyph x={mark.x - r - 7} y={mark.y - r} /> : null}
                <text x={mark.x} y={mark.y + r + 18} textAnchor="middle" fontSize="9.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                  #{mark.row.pull_request.pull_request_id}
                </text>
                <text x={mark.x} y={mark.y + r + 29} textAnchor="middle" fontSize="8" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.6">
                  {mark.change === null ? 'size not served' : `${mark.change.toLocaleString('en-US')} lines`}
                </text>
                {mark.absences.length === 0 ? null : (
                  <text x={mark.x} y={mark.y + r + 39} textAnchor="middle" fontSize="8" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.6" fontStyle="italic">
                    {mark.absences.join(' · ')}
                  </text>
                )}
                <title>{markLabel(mark)}</title>
              </g>
            );
          })}
        </svg>
        <p
          aria-live="polite"
          className={cn(
            'pointer-events-none sticky bottom-0 left-0 z-[2] border-t border-edge-subtle bg-surface-0/85 px-3 py-1.5 font-mono text-3xs text-text-secondary',
            hoveredMark === null && 'text-text-muted',
          )}
        >
          {hoveredMark === null
            ? 'hover a mark to inspect · click or Enter selects · every mark is also a row in the list and the exact table'
            : markLabel(hoveredMark)}
        </p>
      </div>
      <div className="space-y-1">
        <AttentionLegend
          sources={marks.flatMap((mark) => mark.beacons)}
          unevaluated={marks.reduce((sum, mark) => sum + mark.unevaluated, 0)}
        />
        <ul aria-label="Field legend" className="flex flex-wrap gap-x-4 gap-y-1 font-mono text-3xs text-text-muted">
          <li>▭ envelope = registered repository (a container, never a relation)</li>
          <li>□ station = tracked head · track = PR head joined to indexed head</li>
          <li>● size = served line change (log)</li>
          <li>○ hollow · not joined = {UNCORRELATED_SENTENCE}</li>
          <li>dashed ring = provider stale / partial</li>
        </ul>
        <p className="flex flex-wrap items-center gap-2 font-mono text-3xs text-text-muted">
          <span>links = served cross-PR evidence, labelled by basis kind · grade</span>
          <GradeLegend />
          {layout.links.length === 0 ? <span>· {gradeLabel('unavailable')}: no cross-PR link served</span> : null}
        </p>
      </div>
    </div>
  );
}
