import { useMemo, useState, type KeyboardEvent } from 'react';
import type { DeliveryInboxPullRequestV1, DeliveryInboxV1 } from '../../contracts/generated.ts';
import { Corners } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { microsToIso } from './deliveryChrome.tsx';
import { gradeDash, gradeLabel } from './evidence.ts';
import { layoutLanes, threadPath, LANE_GUTTER, type LaneBar, type LaneBead, type LaneZoom } from './lanes.ts';
import { AttentionBeacon, AttentionLegend, GradeLegend, HatchDefs, UnevaluatedGlyph, useMeasuredSize } from './rendererMarks.tsx';
import { attentionCode, headJoin, headJoinSentence, observationWindow, UNCORRELATED_SENTENCE } from './rendererModel.ts';
import type { UmbrellaProjection } from './umbrella.ts';

const ZOOMS: readonly (readonly [LaneZoom, string])[] = [
  ['portfolio', 'Portfolio'],
  ['repository', 'Repository'],
  ['pull_request', 'Pull request'],
];

export const LANE_FIELD_LABEL = 'Delivery lanes · repositories by observed time';

function stamp(micros: number): string {
  return microsToIso(micros).slice(5, 16).replace('T', ' ');
}

/** Luminance ramp for recency within the loaded page: older is dimmer, never hidden. */
function lum(recency: number, floor: number): number {
  return floor + (1 - floor) * recency;
}

/** Attention the daemon could not evaluate, timestamped or not. */
function unevaluatedCount(bar: LaneBar): number {
  return bar.row.attention.filter((item) => item.state === 'unavailable' || item.state === 'denied').length;
}

function barLabel(bar: LaneBar): string {
  const row = bar.row;
  const title = row.pull_request.identity?.title ?? row.pull_request.label;
  const active = bar.beads.flatMap((bead) => (bead.kind === 'attention' && bead.source !== null ? [attentionCode(bead.source)] : []));
  const window = observationWindow(row);
  const unevaluated = unevaluatedCount(bar);
  return [
    `Pull request #${row.pull_request.pull_request_id} · ${title}`,
    window === null ? 'no observation time served' : `observed ${stamp(window.start)} → ${stamp(window.end)} UTC · ${bar.beads.length} observations`,
    active.length === 0 ? 'no active attention' : `active attention ${active.join(', ')}`,
    ...(unevaluated === 0 ? [] : [`${unevaluated} attention source${unevaluated === 1 ? '' : 's'} not evaluated`]),
    row.state,
    bar.hollow ? UNCORRELATED_SENTENCE : 'joined by served evidence',
    headJoinSentence(headJoin(row)),
  ].join(' · ');
}

/** Attention beads at (nearly) one instant stack upward instead of overprinting. */
function stackOffsets(beads: readonly LaneBead[]): readonly number[] {
  return beads.map((bead, index) =>
    bead.kind === 'attention'
      ? beads.slice(0, index).filter((prior) => prior.kind === 'attention' && Math.abs(prior.x - bead.x) < 6).length * 10
      : 0,
  );
}

function Bead({ bead, y, showCode, lift }: { bead: LaneBead; y: number; showCode: boolean; lift: number }) {
  const opacity = lum(bead.recency, 0.45);
  switch (bead.kind) {
    case 'attention':
      return bead.source === null ? null : (
        <g opacity={opacity}>
          <AttentionBeacon x={bead.x} y={y - 8 - lift} source={bead.source} showCode={showCode} />
        </g>
      );
    case 'unevaluated':
      return (
        <g opacity={opacity}>
          <UnevaluatedGlyph x={bead.x} y={y - 8} />
        </g>
      );
    case 'provider_read':
      return <line x1={bead.x} y1={y - 6} x2={bead.x} y2={y + 6} stroke="var(--raw-graph-text)" strokeOpacity={0.75 * opacity} strokeWidth="1" />;
    default: {
      const unhandled: never = bead.kind;
      return unhandled;
    }
  }
}

/**
 * The Delivery lanes field: time on X, registered repositories as lanes on Y,
 * each admitted PR a thin bar over its observation window inside a full-width
 * 44px row band, with CI / review beads and thin threads only where a served
 * basis joins two PRs. Every bar is also a row in the list and the table.
 */
export function LaneField({
  inbox,
  rows,
  projection,
  zoom,
  focusProject,
  selectedRowId,
  onSelectRow,
  onZoom,
}: {
  inbox: DeliveryInboxV1;
  rows: readonly DeliveryInboxPullRequestV1[];
  projection: UmbrellaProjection;
  zoom: LaneZoom;
  focusProject: string | null;
  selectedRowId: string | null;
  onSelectRow: (row: DeliveryInboxPullRequestV1) => void;
  onZoom: (zoom: LaneZoom) => void;
}) {
  const [ref, size] = useMeasuredSize({ width: 900, height: 520 });
  const layout = useMemo(
    () => layoutLanes(inbox, rows, projection, size, { zoom, project: focusProject, pullRequest: selectedRowId }),
    [inbox, rows, projection, size, zoom, focusProject, selectedRowId],
  );
  const [hovered, setHovered] = useState<string | null>(null);
  const [focused, setFocused] = useState<string | null>(null);
  const bars = layout.lanes.flatMap((lane) => lane.bars);
  const anchor = hovered ?? selectedRowId;
  const lit = useMemo(() => {
    if (anchor === null) return null;
    const set = new Set([anchor]);
    for (const { link } of layout.threads) {
      if (link.from === anchor) set.add(link.to);
      if (link.to === anchor) set.add(link.from);
    }
    return set;
  }, [anchor, layout.threads]);
  const litLanes = lit === null ? null : new Set(bars.filter((bar) => lit.has(bar.row.id)).map((bar) => bar.row.project_id));
  const showCodes = bars.length <= 12;
  const hoveredBar = bars.find((bar) => bar.row.id === hovered) ?? null;
  const activate = (event: KeyboardEvent<SVGGElement>, run: () => void) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      run();
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div role="group" aria-label="Semantic zoom" className="flex flex-wrap items-center gap-2">
        <span className="td-legend">semantic zoom</span>
        <div className="flex border border-edge-subtle">
          {ZOOMS.map(([level, label]) => {
            const disabled = (level === 'repository' && focusProject === null) || (level === 'pull_request' && selectedRowId === null);
            return (
              <button
                key={level}
                type="button"
                aria-pressed={zoom === level}
                disabled={disabled}
                title={disabled ? (level === 'repository' ? 'Scope a project first' : 'Select a pull request first') : undefined}
                className={cn(
                  'td-hit px-3 text-3xs uppercase tracking-[0.12em]',
                  zoom === level ? 'bg-surface-2 text-text-primary' : 'text-text-muted',
                  disabled && 'cursor-not-allowed opacity-50',
                )}
                onClick={() => onZoom(level)}
              >
                {label}
              </button>
            );
          })}
        </div>
        <span className="font-mono text-3xs text-text-muted">
          {layout.lanes.length} lanes · {bars.length} bars drawn · {layout.lanes.filter((lane) => lane.compressed).length} compressed · {layout.threads.length} threads
        </span>
      </div>
      <div ref={ref} className="td-optic td-grain relative min-h-72 flex-1 overflow-auto" data-field="delivery-lanes">
        <Corners tone="signal" />
        <svg width={layout.width} height={layout.height} className="relative z-[1] block" role="group" aria-label={LANE_FIELD_LABEL}>
          <defs>
            <HatchDefs id="lane-hatch" />
          </defs>
          <text x="12" y="18" fontSize="10" fontFamily="var(--font-mono)" letterSpacing="0.14em" fill="var(--raw-graph-text)" fillOpacity="0.7">
            OBSERVED TIME · UTC
          </text>
          {layout.ticks.map((tick) => (
            <g key={tick.at} aria-hidden>
              <line x1={tick.x} y1={24} x2={tick.x} y2={layout.height} stroke="var(--raw-graph-dim)" strokeWidth="1" />
              <text x={tick.x} y={18} textAnchor="middle" fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.75">
                {stamp(tick.at)}
              </text>
            </g>
          ))}

          {layout.lanes.map((lane, index) => {
            const max = Math.max(1, ...lane.bins.map((bin) => bin.count));
            const dim = litLanes !== null && !litLanes.has(lane.project.project_id);
            return (
              <g
                key={lane.project.project_id}
                data-lane={lane.project.project_id}
                data-lane-compressed={lane.compressed}
                opacity={dim ? 0.45 : 1}
                className="transition-opacity motion-reduce:transition-none"
              >
                <rect x={0} y={lane.y} width={layout.width} height={lane.height} fill="var(--raw-graph-substrate)" fillOpacity={index % 2 === 0 ? 0.5 : 0.25} />
                <line x1={0} y1={lane.y} x2={layout.width} y2={lane.y} stroke="var(--raw-graph-edge)" strokeOpacity="0.6" />
                <text x="12" y={lane.y + 15} fontSize="11" fontFamily="var(--font-mono)" letterSpacing="0.12em" fill="var(--raw-graph-text)">
                  {lane.project.label.toUpperCase()}
                </text>
                <text x="12" y={lane.y + 27} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.65">
                  {`${lane.summary.admitted} PRs · ${lane.summary.active} active · ${lane.summary.stale} stale · ${lane.summary.correlated} joined`}
                </text>
                {lane.height > 40 ? (
                  <text x="12" y={lane.y + 39} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.5">
                    {`provider ${lane.project.provider_state.replaceAll('_', ' ')}`}
                  </text>
                ) : null}
                {lane.absence !== null ? (
                  <g>
                    <rect x={layout.x0} y={lane.y + 5} width={layout.x1 - layout.x0} height={lane.height - 10} fill="url(#lane-hatch)" stroke="var(--raw-graph-edge)" strokeDasharray="1 5" />
                    <text x={layout.x0 + 8} y={lane.y + lane.height / 2 + 3} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                      {lane.absence}
                    </text>
                  </g>
                ) : null}
                {lane.compressed ? (
                  <g aria-hidden>
                    {lane.bins.map((bin) =>
                      bin.count === 0 ? null : (
                        <rect
                          key={bin.x0}
                          x={bin.x0 + 1}
                          y={lane.y + lane.height - 6 - (bin.count / max) * (lane.height - 14)}
                          width={Math.max(1, bin.x1 - bin.x0 - 2)}
                          height={(bin.count / max) * (lane.height - 14)}
                          fill="var(--raw-graph-accent)"
                          fillOpacity="0.45"
                        />
                      ),
                    )}
                    <text x={layout.x1} y={lane.y + 12} textAnchor="end" fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.7">
                      {`compressed · ${lane.summary.admitted} PRs · ${lane.bins.reduce((sum, bin) => sum + bin.count, 0)} observations`}
                    </text>
                  </g>
                ) : null}
              </g>
            );
          })}

          {layout.omitted === null ? null : (
            <g data-lane="omitted">
              <rect x={0} y={layout.omitted.y} width={layout.width} height={layout.omitted.height} fill="url(#lane-hatch)" />
              <text x="12" y={layout.omitted.y + 20} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                {`${layout.omitted.count} registered project${layout.omitted.count === 1 ? '' : 's'} omitted · no indexed head yet · no pull request can be joined`}
              </text>
            </g>
          )}

          <g aria-hidden>
            {layout.threads.map(({ link, from, to }, index) => {
              const incident = anchor !== null && (link.from === anchor || link.to === anchor);
              const path = threadPath(from, to);
              const slot = layout.threads.slice(0, index).filter((prior) => prior.link.from === link.from).length;
              const down = to.y > from.y;
              return (
                <g key={link.id} opacity={lit === null || incident ? 1 : 0.2} data-thread={link.kind} data-grade={link.grade}>
                  {incident ? <path d={path} fill="none" stroke="var(--raw-graph-accent)" strokeOpacity="0.16" strokeWidth="6" /> : null}
                  <path
                    d={path}
                    fill="none"
                    stroke={incident ? 'var(--raw-graph-accent)' : 'var(--raw-graph-text)'}
                    strokeOpacity={incident ? 0.95 : 0.7}
                    strokeWidth={incident ? 1.4 : 1}
                    strokeDasharray={gradeDash(link.grade)}
                  />
                  <text x={from.x + 6} y={from.y + (down ? 13 + slot * 11 : -7 - slot * 11)} fontSize="9" fontFamily="var(--font-mono)" letterSpacing="0.06em" fill="var(--raw-graph-text)" fillOpacity="0.85">
                    {`${link.code} · ${gradeLabel(link.grade)}`}
                  </text>
                </g>
              );
            })}
          </g>

          {bars.map((bar) => {
            const selected = bar.row.id === selectedRowId;
            const dim = lit !== null && !lit.has(bar.row.id);
            const width = Math.max(6, bar.x1 - bar.x0);
            const x = bar.x0 - (bar.x1 - bar.x0 < 6 ? 3 : 0);
            const lifts = stackOffsets(bar.beads);
            return (
              <g key={bar.row.id} opacity={dim ? 0.35 : 1} className="transition-opacity motion-reduce:transition-none">
                <g
                  role="button"
                  tabIndex={0}
                  data-delivery-mark={bar.row.id}
                  aria-label={barLabel(bar)}
                  aria-pressed={selected}
                  className="cursor-pointer outline-none"
                  onClick={() => onSelectRow(bar.row)}
                  onKeyDown={(event) => activate(event, () => onSelectRow(bar.row))}
                  onMouseEnter={() => setHovered(bar.row.id)}
                  onMouseLeave={() => setHovered(null)}
                  onFocus={() => setFocused(bar.row.id)}
                  onBlur={() => setFocused(null)}
                >
                  <rect
                    data-hit-band
                    x={0}
                    y={bar.y - layout.rowHeight / 2}
                    width={layout.width}
                    height={layout.rowHeight}
                    fill={selected ? 'var(--raw-graph-accent)' : 'transparent'}
                    fillOpacity={selected ? 0.05 : 0}
                  />
                  {focused === bar.row.id ? (
                    <rect x={1} y={bar.y - layout.rowHeight / 2 + 1} width={layout.width - 2} height={layout.rowHeight - 2} fill="none" stroke="var(--raw-graph-accent)" strokeWidth="2" />
                  ) : null}
                  {selected ? (
                    <>
                      <rect x={x - 4} y={bar.y - 8} width={width + 8} height={16} fill="none" stroke="var(--raw-graph-accent)" strokeOpacity="0.18" strokeWidth="6" />
                      <rect x={0} y={bar.y - layout.rowHeight / 2} width={2} height={layout.rowHeight} fill="var(--raw-graph-accent)" />
                    </>
                  ) : null}
                  <rect
                    x={x}
                    y={bar.y - 3}
                    width={width}
                    height={6}
                    fill={selected ? 'var(--raw-graph-accent)' : bar.hollow ? 'none' : 'var(--raw-graph-accent)'}
                    fillOpacity={selected ? 1 : bar.hollow ? 0 : lum(bar.recency, 0.2) * 0.85}
                    stroke="var(--raw-graph-accent)"
                    strokeOpacity={selected ? 1 : lum(bar.recency, 0.4)}
                    strokeWidth={selected ? 1.5 : 1}
                    strokeDasharray={bar.row.state === 'current' ? undefined : '2 2'}
                  />
                  {bar.beads.map((bead, index) => (
                    <Bead key={`${bead.kind}:${index}`} bead={bead} y={bar.y} showCode={showCodes} lift={lifts[index] ?? 0} />
                  ))}
                  <text x={bar.x0 + width + 8} y={bar.y + 3.5} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                    {`#${bar.row.pull_request.pull_request_id}`}
                    <tspan fillOpacity="0.6">
                      {bar.undated ? ' · no observation time served' : bar.hollow ? ' · not joined' : ''}
                    </tspan>
                  </text>
                </g>
                {bar.tracks.map((track) => (
                  <g key={track.id} data-track={track.id}>
                    <text x={LANE_GUTTER - 6} y={track.y + 3} textAnchor="end" fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.7">
                      {track.label}
                    </text>
                    {track.absence === null ? (
                      <line x1={layout.x0} y1={track.y} x2={layout.x1} y2={track.y} stroke="var(--raw-graph-dim)" />
                    ) : (
                      <g>
                        <rect x={layout.x0} y={track.y - 7} width={layout.x1 - layout.x0} height={14} fill="url(#lane-hatch)" />
                        <text x={layout.x0 + 6} y={track.y + 3} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                          {`NO EVIDENCE · ${track.absence}`}
                        </text>
                      </g>
                    )}
                    {track.beads.map((bead, index) => (
                      <Bead key={`${bead.kind}:${index}`} bead={bead} y={track.y} showCode lift={stackOffsets(track.beads)[index] ?? 0} />
                    ))}
                  </g>
                ))}
                <title>{barLabel(bar)}</title>
              </g>
            );
          })}
        </svg>
        <p
          aria-live="polite"
          className={cn(
            'pointer-events-none sticky bottom-0 left-0 z-[2] border-t border-edge-subtle bg-surface-0/85 px-3 py-1.5 font-mono text-3xs text-text-secondary',
            hoveredBar === null && 'text-text-muted',
          )}
        >
          {hoveredBar === null
            ? 'hover a row to inspect · click or Enter selects and zooms to the pull request · every bar is also a row in the list and the exact table'
            : barLabel(hoveredBar)}
        </p>
      </div>
      <div className="space-y-1">
        <AttentionLegend
          sources={bars.flatMap((bar) => bar.beads.flatMap((bead) => (bead.kind === 'attention' && bead.source !== null ? [bead.source] : [])))}
          unevaluated={bars.reduce((sum, bar) => sum + unevaluatedCount(bar), 0)}
        />
        <ul aria-label="Field legend" className="flex flex-wrap gap-x-4 gap-y-1 font-mono text-3xs text-text-muted">
          <li>▬ bar = daemon observation window, not PR lifetime · the inbox serves no opened or merged time</li>
          <li>luminance = recency of the newest observation within this loaded page</li>
          <li>| provider read · □ hollow = {UNCORRELATED_SENTENCE} · dashed bar = provider stale / partial</li>
          {layout.hiddenLinks > 0 ? <li>{layout.hiddenLinks} served link{layout.hiddenLinks === 1 ? '' : 's'} end in a compressed or undrawn lane</li> : null}
        </ul>
        <p className="flex flex-wrap items-center gap-2 font-mono text-3xs text-text-muted">
          <span>threads = served cross-PR evidence · basis kind · printed grade</span>
          <GradeLegend />
        </p>
      </div>
    </div>
  );
}
