import { useState } from 'react';
import type { DeliveryInboxV1 } from '../../contracts/generated.ts';
import { cn } from '../../ui/cn.ts';
import { GradeMark, microsToIso } from './deliveryChrome.tsx';
import { gradeDash, gradeLabel } from './evidence.ts';
import { pullRequestNumberLabel } from './deliveryReading.ts';
import { projectFor } from './inboxFilter.ts';
import { AttentionLegend, GradeLegend, HATCH_STYLE, useMeasuredSize } from './rendererMarks.tsx';
import { attentionCode } from './rendererModel.ts';
import {
  gradeSummary,
  stationStateLabel,
  type TransitLink,
  type TransitModel,
  type TransitStation,
} from './transit.ts';

const ITEM_LIMIT = 6;

function utc(micros: number): string {
  return microsToIso(micros).slice(5, 16).replace('T', ' ');
}

function Beacon({ label }: { label: string }) {
  return (
    <span className="inline-flex items-center gap-1 font-mono text-3xs text-alert">
      <svg aria-hidden width="9" height="9" viewBox="-5 -5 10 10">
        <path d="M 0 -4 L 4 3 L -4 3 Z" fill="currentColor" />
      </svg>
      {label}
    </span>
  );
}

function Connector({ link }: { link: TransitLink }) {
  return (
    <li
      aria-label={`${link.from} to ${link.to} · ${gradeLabel(link.grade)} · ${link.basis}`}
      className="flex shrink-0 items-center justify-center gap-2 py-1 xl:w-20 xl:flex-col xl:py-0"
    >
      <svg aria-hidden width="56" height="8" viewBox="0 0 56 8" className="max-xl:rotate-90">
        <line x1="0" y1="4" x2="48" y2="4" stroke="var(--raw-graph-text)" strokeOpacity="0.7" strokeWidth="1.4" strokeDasharray={gradeDash(link.grade)} />
        <path d="M 48 0.5 L 55 4 L 48 7.5 Z" fill="var(--raw-graph-text)" fillOpacity="0.7" />
      </svg>
      <span className="text-center font-mono text-3xs leading-tight tracking-[0.06em] text-text-muted">
        {gradeLabel(link.grade)}
        <br />
        {link.basis}
      </span>
    </li>
  );
}

function StationPlate({
  entry,
  index,
  selectedEpisodeId,
  onSelectEpisode,
}: {
  entry: TransitStation;
  index: number;
  selectedEpisodeId: string | null;
  onSelectEpisode: ((episodeId: string) => void) | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const shown = entry.items.slice(0, ITEM_LIMIT);
  const branchCounts = entry.branches.filter((branch) => branch.identities.length > 0);
  return (
    <li
      aria-label={`${entry.title} · ${stationStateLabel(entry.state)} · ${gradeSummary(entry)}`}
      data-station={entry.id}
      data-station-state={entry.state}
      className={cn(
        'flex min-w-0 flex-1 flex-col border bg-surface-1',
        entry.state === 'evidence' ? 'border-edge-strong' : 'border-dashed border-edge-subtle',
      )}
    >
      <div aria-hidden className="h-1.5 border-b border-edge-subtle" style={entry.state === 'no_evidence' ? HATCH_STYLE : undefined}>
        {entry.state === 'no_evidence' ? null : (
          <svg width="100%" height="6" preserveAspectRatio="none" className="block">
            <line x1="0" y1="3" x2="100%" y2="3" stroke="var(--raw-graph-accent)" strokeWidth="2" strokeDasharray={gradeDash(entry.grade)} />
          </svg>
        )}
      </div>
      <header className="border-b border-edge-subtle px-3 py-2">
        <p className="td-legend text-text-muted">
          {String(index + 1).padStart(2, '0')} · {stationStateLabel(entry.state)}
        </p>
        <h3 className="td-title mt-1.5 text-text-primary">{entry.title}</h3>
        <p className="mt-1 font-mono text-3xs text-text-secondary">{gradeSummary(entry)}</p>
        <p className="mt-0.5 font-mono text-3xs text-text-muted">
          {entry.span === null ? 'undated' : entry.span.start === entry.span.end ? utc(entry.span.start) : `${utc(entry.span.start)} → ${utc(entry.span.end)}`}
        </p>
      </header>
      {entry.state === 'no_evidence' ? (
        <div className="flex-1 px-3 py-3" style={HATCH_STYLE}>
          <p className="td-legend inline-block bg-surface-0 px-1 py-0.5 text-text-primary">no evidence</p>
          <ul className="mt-2 space-y-1">
            {entry.reasons.map((reason) => (
              <li key={reason} className="bg-surface-0/80 px-1 text-body leading-snug text-text-secondary">{reason}</li>
            ))}
          </ul>
        </div>
      ) : (
        <div className="flex-1 px-3 py-2">
          {entry.state === 'served_empty' ? (
            <p className="td-legend text-text-muted">served empty</p>
          ) : null}
          <ul className="space-y-1.5">
            {shown.map((item) => {
              const content = (
                <>
                  <span className="flex flex-wrap items-center gap-2">
                    {item.attention === null ? <GradeMark grade={item.grade} source={item.source} /> : <Beacon label={attentionCode(item.attention)} />}
                    {item.at === null ? null : <span className="font-mono text-3xs text-text-muted">{utc(item.at)} {item.timeKind === 'observed' ? 'OBS' : 'EVT'}</span>}
                  </span>
                  <span className="block break-words text-body leading-snug text-text-primary">{item.label}</span>
                  <span className="block break-words font-mono text-3xs text-text-muted">{item.detail}</span>
                </>
              );
              return (
                <li key={item.id}>
                  {item.episodeId !== null && onSelectEpisode !== null ? (
                    <button
                      type="button"
                      aria-pressed={selectedEpisodeId === item.episodeId}
                      className={cn(
                        'relative block min-h-11 w-full px-1.5 py-1 text-left hover:bg-surface-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent',
                        selectedEpisodeId === item.episodeId && 'bg-surface-2',
                      )}
                      onClick={() => onSelectEpisode(item.episodeId as string)}
                    >
                      {selectedEpisodeId === item.episodeId ? <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" /> : null}
                      {content}
                    </button>
                  ) : (
                    <div className="px-1.5 py-1">{content}</div>
                  )}
                </li>
              );
            })}
          </ul>
          {entry.items.length > ITEM_LIMIT ? (
            <p className="mt-1 font-mono text-3xs text-text-muted">+{entry.items.length - ITEM_LIMIT} more in the exact table</p>
          ) : null}
          {entry.reasons.length === 0 ? null : (
            <ul className="mt-2 space-y-1 border-t border-edge-subtle pt-2">
              {entry.reasons.map((reason) => (
                <li key={reason} className="px-1 py-0.5 text-body leading-snug text-text-muted" style={HATCH_STYLE}>
                  {reason}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {branchCounts.length === 0 ? null : (
        <div className="border-t border-edge-subtle px-3 py-2">
          <button
            type="button"
            aria-expanded={expanded}
            className="min-h-11 w-full text-left font-mono text-3xs text-text-secondary hover:text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
            onClick={() => setExpanded((value) => !value)}
          >
            {expanded ? '▾' : '▸'} branches · {branchCounts.map((branch) => `${branch.identities.length} ${branch.kind}${branch.identities.length === 1 ? '' : 's'}`).join(' · ')}
          </button>
          {expanded ? (
            <ul className="mt-1 space-y-0.5">
              {branchCounts.flatMap((branch) =>
                branch.identities.map((identity) => (
                  <li key={`${branch.kind}:${identity}`} className="break-all font-mono text-3xs text-text-muted">
                    {branch.kind} · {identity}
                  </li>
                )),
              )}
            </ul>
          ) : null}
        </div>
      )}
    </li>
  );
}

/** Each station's dated span on one shared axis, beneath the chain. */
function TransitRuler({ model }: { model: TransitModel }) {
  const [ref, size] = useMeasuredSize({ width: 720, height: 0 });
  const span = model.span;
  const x0 = 120;
  const x1 = Math.max(x0 + 40, size.width - 16);
  const at = (micros: number) =>
    span === null || span.end === span.start ? (x0 + x1) / 2 : x0 + ((micros - span.start) / (span.end - span.start)) * (x1 - x0);
  const rowHeight = 16;
  return (
    <div ref={ref} className="mt-3" aria-label="Transit time ruler">
      <svg width={size.width} height={model.stations.length * rowHeight + 18} className="block" aria-hidden>
        {model.stations.map((entry, index) => {
          const y = index * rowHeight + 9;
          return (
            <g key={entry.id}>
              <text x="0" y={y + 3} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.7">
                {entry.title.toUpperCase()}
              </text>
              <line x1={x0} y1={y} x2={x1} y2={y} stroke="var(--raw-graph-dim)" strokeWidth="1" />
              {entry.span === null ? (
                <text x={x0 + 4} y={y + 3} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.5">
                  undated
                </text>
              ) : (
                <rect
                  x={at(entry.span.start) - 2}
                  y={y - 3}
                  width={Math.max(4, at(entry.span.end) - at(entry.span.start) + 4)}
                  height="6"
                  fill="var(--raw-graph-accent)"
                  fillOpacity="0.8"
                />
              )}
            </g>
          );
        })}
        {span === null ? null : (
          <g>
            <text x={x0} y={model.stations.length * rowHeight + 15} fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.6">
              {utc(span.start)} UTC
            </text>
            <text x={x1} y={model.stations.length * rowHeight + 15} textAnchor="end" fontSize="10" fontFamily="var(--font-mono)" fill="var(--raw-graph-text)" fillOpacity="0.6">
              {utc(span.end)} UTC
            </text>
          </g>
        )}
      </svg>
    </div>
  );
}

/**
 * Renderer B: the selected PR as four stations. Stations are ordered by the
 * delivery chain, and the ruler below places each one's recorded span in time.
 */
export function TransitField({
  model,
  selectedEpisodeId,
  onSelectEpisode,
}: {
  model: TransitModel;
  selectedEpisodeId: string | null;
  onSelectEpisode: ((episodeId: string) => void) | null;
}) {
  const sources = model.stations.flatMap((entry) => entry.items.flatMap((item) => (item.attention === null ? [] : [item.attention])));
  const next = model.stations.find((entry) => entry.id === 'next');
  return (
    <div className="td-optic relative flex min-w-0 flex-col p-3" data-field="delivery-transit">
      <ol aria-label="Delivery transit" className="flex flex-col items-stretch xl:flex-row">
        {model.stations.map((entry, index) => {
          const link = model.links[index];
          return [
            <StationPlate
              key={entry.id}
              entry={entry}
              index={index}
              selectedEpisodeId={selectedEpisodeId}
              onSelectEpisode={onSelectEpisode}
            />,
            link === undefined ? null : <Connector key={`${link.from}->${link.to}`} link={link} />,
          ];
        })}
      </ol>
      <TransitRuler model={model} />
      <div className="mt-2 space-y-1">
        <AttentionLegend
          sources={[...new Set(sources)]}
          unevaluated={next?.reasons.filter((reason) => reason.includes('not evaluated')).length ?? 0}
        />
        <p className="flex flex-wrap items-center gap-2 font-mono text-3xs text-text-muted">
          <span>hatched = NO EVIDENCE, never skipped · plate stripe and links stroked by grade</span>
          <GradeLegend />
        </p>
      </div>
    </div>
  );
}

/** The inbox folded to a rail beside a journey: one row per admitted PR. */
export function CompactRail({
  inbox,
  selectedId,
  onSelect,
}: {
  inbox: DeliveryInboxV1;
  selectedId: string;
  onSelect: (id: string) => void;
}) {
  return (
    <nav aria-label="Inbox rail" className="border-r border-edge-subtle bg-surface-1">
      <p className="td-legend border-b border-edge-subtle px-3 py-2">inbox · {inbox.pull_requests.length} admitted</p>
      <ul>
        {inbox.pull_requests.map((row) => {
          const active = row.attention.filter((item) => item.state === 'active').length;
          return (
            <li key={row.id} className="border-b border-edge-subtle">
              <button
                type="button"
                aria-current={row.id === selectedId ? 'page' : undefined}
                className={cn(
                  'relative flex min-h-11 w-full flex-col justify-center px-3 py-1 text-left hover:bg-surface-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-accent',
                  row.id === selectedId && 'bg-surface-2',
                )}
                onClick={() => onSelect(row.id)}
              >
                {row.id === selectedId ? <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" /> : null}
                <span className="truncate font-mono text-3xs text-text-secondary">
                  {projectFor(inbox, row.project_id)?.label ?? row.project_id} {pullRequestNumberLabel(row.pull_request)}
                </span>
                <span className="flex items-center gap-2 truncate text-3xs text-text-muted">
                  {row.state}
                  {active > 0 ? <Beacon label={`${active}`} /> : null}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
