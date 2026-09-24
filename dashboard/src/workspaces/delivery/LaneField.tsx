import { useMemo, useState, type KeyboardEvent } from 'react';
import type { DeliveryInboxPullRequestV1, DeliveryInboxV1 } from '../../contracts/generated.ts';
import { Corners } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { microsToIso } from './deliveryChrome.tsx';
import { gradeDash } from './evidence.ts';
import type { JourneyModel } from './journey.ts';
import { layoutLanes, threadPath, LANE_GUTTER, type LaneBar, type LaneBead, type LaneZoom } from './lanes.ts';
import { AttentionBeacon, AttentionLegend, GradeLegend, HatchDefs, UnevaluatedGlyph, useMeasuredSize } from './rendererMarks.tsx';
import { attentionCode, headJoin, headJoinSentence, observationWindow, UNCORRELATED_SENTENCE } from './rendererModel.ts';
import type { UmbrellaProjection } from './umbrella.ts';

const ZOOMS: readonly (readonly [LaneZoom, string])[] = [
  ['portfolio', 'Portfolio'],
  ['repository', 'Repository'],
  ['pull_request', 'Pull request'],
];

function stamp(micros: number): string {
  return microsToIso(micros).slice(5, 16).replace('T', ' ');
}

function barLabel(bar: LaneBar): string {
  const row = bar.row;
  const title = row.pull_request.identity?.title ?? row.pull_request.label;
  const active = bar.beads.flatMap((bead) => (bead.kind === 'attention' && bead.source !== null ? [attentionCode(bead.source)] : []));
  const window = observationWindow(row);
  return [
    `Pull request #${row.pull_request.pull_request_id} · ${title}`,
    window === null ? 'no observation time served' : `observed ${stamp(window.start)} → ${stamp(window.end)} UTC · ${bar.beads.length} observations`,
    active.length === 0 ? 'no active attention' : `active attention ${active.join(', ')}`,
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

function Bead({ bead, y, showCode, lift = 0 }: { bead: LaneBead; y: number; showCode: boolean; lift?: number }) {
  switch (bead.kind) {
    case 'attention':
      return bead.source === null ? null : <AttentionBeacon x={bead.x} y={y - 8 - lift} source={bead.source} showCode={showCode} />;
    case 'unevaluated':
      return <UnevaluatedGlyph x={bead.x} y={y - 8} />;
    case 'provider_read':
      return <line x1={bead.x} y1={y - 6} x2={bead.x} y2={y + 6} stroke="var(--raw-graph-text)" strokeOpacity="0.7" strokeWidth="1" />;
    case 'event':
      return <circle cx={bead.x} cy={y} r={3.5} fill="var(--raw-graph-accent)" />;
    case 'observed':
      return <path d={`M ${bead.x} ${y - 4} L ${bead.x + 4} ${y} L ${bead.x} ${y + 4} L ${bead.x - 4} ${y} Z`} fill="none" stroke="var(--raw-graph-accent)" strokeWidth="1.2" />;
    default: {
      const unhandled: never = bead.kind;
      return unhandled;
    }
  }
}

/**
 * Renderer C: time on X, registered repositories as lanes on Y, each admitted
 * PR a bar over its observation window with CI / review beads, and thin
 * threads only where a served basis joins two PRs.
 */
export function LaneField({
  inbox,
  rows,
  projection,
  zoom,
  focusProject,
  selectedRowId,
  journey,
  selectedEpisodeId,
  onSelectRow,
  onSelectEpisode,
  onZoom,
}: {
  inbox: DeliveryInboxV1;
  rows: readonly DeliveryInboxPullRequestV1[];
  projection: UmbrellaProjection;
  zoom: LaneZoom;
  focusProject: string | null;
  selectedRowId: string | null;
  journey: JourneyModel | null;
  selectedEpisodeId: string | null;
  onSelectRow: (row: DeliveryInboxPullRequestV1) => void;
  onSelectEpisode: ((episodeId: string) => void) | null;
  onZoom: (zoom: LaneZoom) => void;
}) {
  const [ref, size] = useMeasuredSize({ width: 900, height: 520 });
  const layout = useMemo(
    () => layoutLanes(inbox, rows, projection, size, { zoom, project: focusProject, pullRequest: selectedRowId, journey }),
    [inbox, rows, projection, size, zoom, focusProject, selectedRowId, journey],
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
  const showCodes = bars.length <= 12;
  const hoveredBar = bars.find((bar) => bar.row.id === hovered) ?? null;
  const canRepository = focusProject !== null;
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
            const disabled = (level === 'repository' && !canRepository) || (level === 'pull_request' && selectedRowId === null);
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
      <div ref={ref} className="td-optic td-grain relative min-h-72 flex-1 overflow-auto" data-delivery-renderer="lanes">
        <Corners tone="signal" />
        <svg width={layout.width} height={layout.height} className="relative z-[1] block" role="group" aria-label="Dense delivery field · repositories by observed time">
          <defs>
            <HatchDefs id="lane-hatch" />
          </defs>
          <text x="12" y="18" fontSize="9" fontFamily="var(--font-mono)" letterSpacing="0.14em" fill="var(--raw-graph-text)" fillOpacity="0.7">
            OBSERVED TIME · UTC
          </text>
          {layout.ticks.map((tick) => (
            <g key={tick.at} aria-hidden>
              <line x1={tick.x} y1={24} x2={tick.x} y2={layout.height} stroke="var(--raw-graph-dim)" strokeWidth="1" />
              <text x={tick.x} y={18} textAnchor="middle" fontSize="9" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.75">
                {stamp(tick.at)}
              </text>
            </g>
          ))}

          {layout.lanes.map((lane, index) => {
            const max = Math.max(1, ...lane.bins.map((bin) => bin.count));
            return (
              <g key={lane.project.project_id} data-lane={lane.project.project_id} data-lane-compressed={lane.compressed}>
                <rect x={0} y={lane.y} width={layout.width} height={lane.height} fill="var(--raw-graph-substrate)" fillOpacity={index % 2 === 0 ? 0.5 : 0.25} />
                <line x1={0} y1={lane.y} x2={layout.width} y2={lane.y} stroke="var(--raw-graph-edge)" strokeOpacity="0.6" />
                <text x="12" y={lane.y + 15} fontSize="10.5" fontFamily="var(--font-mono)" letterSpacing="0.12em" fill="var(--raw-graph-text)">
                  {lane.project.label.toUpperCase()}
                </text>
                <text x="12" y={lane.y + 27} fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.65">
                  {`${lane.summary.admitted} PRs · ${lane.summary.active} active · ${lane.summary.stale} stale · ${lane.summary.correlated} joined`}
                </text>
                {lane.height > 40 ? (
                  <text x="12" y={lane.y + 39} fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.5">
                    {`provider ${lane.project.provider_state.replaceAll('_', ' ')}`}
                  </text>
                ) : null}
                {lane.absence !== null ? (
                  <g>
                    <rect x={layout.x0} y={lane.y + 5} width={layout.x1 - layout.x0} height={lane.height - 10} fill="url(#lane-hatch)" stroke="var(--raw-graph-edge)" strokeDasharray="1 5" />
                    <text x={layout.x0 + 8} y={lane.y + lane.height / 2 + 3} fontSize="9" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
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
                    <text x={layout.x1} y={lane.y + 12} textAnchor="end" fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.7">
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
              <text x="12" y={layout.omitted.y + 20} fontSize="9" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                {`${layout.omitted.count} registered project${layout.omitted.count === 1 ? '' : 's'} omitted · no indexed head yet · no pull request can be joined`}
              </text>
            </g>
          )}

          <g aria-hidden>
            {layout.threads.map(({ link, from, to }) => {
              const on = lit === null || (lit.has(link.from) && lit.has(link.to) && (link.from === anchor || link.to === anchor));
              return (
                <g key={link.id} opacity={on ? 1 : 0.2} data-thread={link.kind}>
                  <path d={threadPath(from, to)} fill="none" stroke="var(--raw-graph-text)" strokeOpacity="0.7" strokeWidth="1" strokeDasharray={gradeDash(link.grade)} />
                  <text x={from.x + 6} y={from.y + (to.y > from.y ? 12 : -6)} fontSize="7.5" fontFamily="var(--font-mono)" letterSpacing="0.08em" fill="var(--raw-graph-text)" fillOpacity="0.8">
                    {link.code}
                  </text>
                </g>
              );
            })}
          </g>

          {bars.map((bar) => {
            const selected = bar.row.id === selectedRowId;
            const dim = lit !== null && !lit.has(bar.row.id);
            const width = Math.max(6, bar.x1 - bar.x0);
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
                  <rect x={layout.x0 - 8} y={bar.y - 14} width={layout.x1 - layout.x0 + 16} height={28} fill="transparent" />
                  {focused === bar.row.id ? (
                    <rect x={bar.x0 - 5} y={bar.y - 9} width={width + 10} height={18} fill="none" stroke="var(--raw-graph-accent)" strokeWidth="2" />
                  ) : null}
                  <rect
                    x={bar.x0 - (bar.x1 - bar.x0 < 6 ? 3 : 0)}
                    y={bar.y - 4}
                    width={width}
                    height={8}
                    fill={selected ? 'var(--raw-graph-accent)' : bar.hollow ? 'none' : 'color-mix(in oklab, var(--raw-graph-accent) 30%, var(--raw-graph-substrate))'}
                    stroke="var(--raw-graph-accent)"
                    strokeWidth={selected ? 2 : 1}
                    strokeDasharray={bar.row.state === 'current' ? undefined : '2 2'}
                  />
                  {bar.beads.map((bead, index) => (
                    <Bead key={`${bead.kind}:${index}`} bead={bead} y={bar.y} showCode={showCodes} lift={lifts[index] ?? 0} />
                  ))}
                  <text x={bar.x0 + width + 8} y={bar.y + 3.5} fontSize="9.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                    {`#${bar.row.pull_request.pull_request_id}`}
                    <tspan fillOpacity="0.6">
                      {bar.undated ? ' · no observation time served' : bar.hollow ? ' · not joined' : ''}
                    </tspan>
                  </text>
                </g>
                {bar.tracks.map((track) => (
                  <g key={track.id} data-track={track.id}>
                    <text x={LANE_GUTTER - 6} y={track.y + 3} textAnchor="end" fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.7">
                      {track.label}
                    </text>
                    {track.absence === null ? (
                      <line x1={layout.x0} y1={track.y} x2={layout.x1} y2={track.y} stroke="var(--raw-graph-dim)" />
                    ) : (
                      <g>
                        <rect x={layout.x0} y={track.y - 7} width={layout.x1 - layout.x0} height={14} fill="url(#lane-hatch)" />
                        <text x={layout.x0 + 6} y={track.y + 3} fontSize="8.5" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)">
                          {`NO EVIDENCE · ${track.absence}`}
                        </text>
                      </g>
                    )}
                    {track.beads.map((bead, index) =>
                      bead.episodeId !== null && onSelectEpisode !== null ? (
                        <g
                          key={`${bead.episodeId}:${index}`}
                          role="button"
                          tabIndex={0}
                          aria-label={`${track.label} · ${bead.label}`}
                          aria-pressed={selectedEpisodeId === bead.episodeId}
                          className="cursor-pointer outline-none focus-visible:outline-2 focus-visible:outline-accent"
                          onClick={() => onSelectEpisode(bead.episodeId as string)}
                          onKeyDown={(event) => activate(event, () => onSelectEpisode(bead.episodeId as string))}
                        >
                          <rect x={bead.x - 10} y={track.y - 10} width={20} height={20} fill="transparent" />
                          {selectedEpisodeId === bead.episodeId ? (
                            <circle cx={bead.x} cy={track.y} r={7} fill="none" stroke="var(--raw-graph-accent)" strokeWidth="2" />
                          ) : null}
                          <Bead bead={bead} y={track.y} showCode />
                        </g>
                      ) : (
                        <Bead key={`${bead.kind}:${index}`} bead={bead} y={track.y} showCode lift={stackOffsets(track.beads)[index] ?? 0} />
                      ),
                    )}
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
            ? 'hover a bar to inspect · click or Enter selects and zooms to the pull request · every bar is also a row in the list and the exact table'
            : barLabel(hoveredBar)}
        </p>
      </div>
      <div className="space-y-1">
        <AttentionLegend
          sources={bars.flatMap((bar) => bar.beads.flatMap((bead) => (bead.kind === 'attention' && bead.source !== null ? [bead.source] : [])))}
          unevaluated={bars.reduce((sum, bar) => sum + bar.beads.filter((bead) => bead.kind === 'unevaluated').length, 0)}
        />
        <ul aria-label="Field legend" className="flex flex-wrap gap-x-4 gap-y-1 font-mono text-3xs text-text-muted">
          <li>▬ bar = daemon observation window, not PR lifetime · the inbox serves no opened or merged time</li>
          <li>| provider read · ● event · ◇ observed (focused PR)</li>
          <li>□ hollow · not joined = {UNCORRELATED_SENTENCE}</li>
          <li>dashed bar = provider stale / partial</li>
          {layout.hiddenLinks > 0 ? <li>{layout.hiddenLinks} served link{layout.hiddenLinks === 1 ? '' : 's'} end in a compressed lane</li> : null}
        </ul>
        <p className="flex flex-wrap items-center gap-2 font-mono text-3xs text-text-muted">
          <span>threads = served cross-PR evidence by basis kind · grade</span>
          <GradeLegend />
        </p>
      </div>
    </div>
  );
}
