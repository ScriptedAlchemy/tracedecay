/**
 * Marks, keys and readouts the provenance cameras draw with.
 *
 * Text is set by the shell's type tiers, not by the drawing's scale: the
 * layout is in screen pixels, so a label is exactly the size its tier names.
 * Row labels take the 14px body tier, legends the 10px engraved tier, and
 * measured values the mono tabular tier. Trust is the one quantity drawn as
 * light: a rail's fill and a glyph's core brighten with trust on one ramp,
 * a fact whose trust the store did not report is a dashed ring, and a
 * withheld fact is violet crosshatch whatever its other readings.
 */
import { useLayoutEffect, useRef, useState, type ReactNode } from 'react';

import type { MemoryReadStatusV1, ProjectMemoryGraphRelationKindV1 } from '../../contracts/generated.ts';
import { cn } from '../../ui/cn';
import { StateChip } from '../../ui/StateChip.tsx';
import { accessWord, relationStyle, type SceneCoverage, type SceneFact, type SceneRelation } from './factScene.ts';

/** Pixel sizes of the tiers, for the layout's text budgets. */
export const TIER_PX = { body: 14, legend: 10, value: 12 } as const;
export type TextTier = keyof typeof TIER_PX;

const TIER_CLASS: Record<TextTier, string> = {
  body: 'text-body',
  legend: 'td-legend',
  value: 'td-value text-xs',
};

/** Trust as light: the one ramp every rail fill and glyph core uses. A floor
 * keeps a low-trust fact a visible mark rather than an absence. */
export function trustOpacity(trust: number): number {
  return Math.round((0.3 + Math.max(0, Math.min(1, trust)) * 0.7) * 100) / 100;
}

const TONE_STROKE: Record<ReturnType<typeof relationStyle>['tone'], string> = {
  signal: 'var(--raw-graph-accent)',
  conflict: 'var(--raw-state-conflicting)',
  stale: 'var(--raw-state-stale)',
  quiet: 'var(--raw-graph-edge)',
};

const TONE_OPACITY: Record<ReturnType<typeof relationStyle>['tone'], number> = {
  signal: 0.7,
  conflict: 0.85,
  stale: 0.8,
  quiet: 0.5,
};

export function relationStroke(kind: ProjectMemoryGraphRelationKindV1) {
  const style = relationStyle(kind);
  return {
    label: style.label,
    stroke: TONE_STROKE[style.tone],
    opacity: TONE_OPACITY[style.tone],
    width: style.tone === 'conflict' ? 1.8 : 1.3,
    dash: style.dash,
    loud: style.tone === 'conflict' || style.tone === 'stale',
  };
}

/** The container's measured size, or the fallback until (or unless) layout
 * reports one: jsdom never does, a browser does on first observation. */
export function useMeasuredBox(fallback: { width: number; height: number }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [box, setBox] = useState(fallback);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const read = () => {
      const rect = element.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return;
      const next = { width: Math.round(rect.width), height: Math.round(rect.height) };
      setBox((current) => (current.width === next.width && current.height === next.height ? current : next));
    };
    read();
    const observer = new ResizeObserver(read);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, box] as const;
}

/** The violet crosshatch a withheld fact is filled with. */
export function HatchDef({ id }: { id: string }) {
  return (
    <pattern id={id} width="4" height="4" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
      <rect width="4" height="4" fill="var(--raw-state-redacted)" opacity={0.25} />
      <line x1="0" y1="0" x2="0" y2="4" stroke="var(--raw-state-locked)" strokeWidth={1.2} />
      <line x1="0" y1="0" x2="4" y2="0" stroke="var(--raw-state-locked)" strokeWidth={0.6} />
    </pattern>
  );
}

/** Tiered text with a surface halo, so it stays legible over a route. */
export function Label({
  x,
  y,
  children,
  tier,
  anchor = 'start',
  tone = 'var(--raw-graph-text)',
  opacity = 0.94,
}: {
  x: number;
  y: number;
  children: ReactNode;
  tier: TextTier;
  anchor?: 'start' | 'middle' | 'end';
  tone?: string;
  opacity?: number;
}) {
  return (
    <text
      x={x}
      y={y}
      textAnchor={anchor}
      fill={tone}
      opacity={opacity}
      stroke="var(--raw-surface-0)"
      strokeWidth={3}
      strokeLinejoin="round"
      paintOrder="stroke"
      data-tier={tier}
      className={cn(TIER_CLASS[tier], 'pointer-events-none transition-opacity duration-[var(--dur-state)]')}
    >
      {children}
    </text>
  );
}

export function trustText(fact: SceneFact): string {
  if (fact.trust != null) return `trust ${fact.trust.toFixed(2)}`;
  return fact.restricted ? `${accessWord(fact.access)} · trust absent` : 'trust absent';
}

/** The focal readout: the inspected fact, else the selected one, else the
 * most wired. It says which of the three it is. */
export function FocalReadout({ fact, role }: { fact: SceneFact | null; role: 'selected' | 'inspecting' | 'hub' }) {
  if (!fact) return null;
  return (
    <div
      className="flex min-w-0 flex-1 items-center gap-2.5 border-l-2 border-accent bg-surface-0/70 py-0.5 pl-2 pr-3"
      data-testid="constellation-focal"
      data-focal-role={role}
    >
      <svg aria-hidden width="20" height="20" viewBox="0 0 20 20" className="shrink-0">
        {fact.restricted ? (
          <>
            <defs>
              <HatchDef id="focal-hatch" />
            </defs>
            <rect x="4" y="4" width="12" height="12" fill="url(#focal-hatch)" stroke="var(--raw-state-locked)" />
          </>
        ) : (
          <circle
            cx="10"
            cy="10"
            r="6"
            fill="var(--raw-graph-accent)"
            fillOpacity={fact.trust == null ? 0 : trustOpacity(fact.trust)}
            stroke="var(--raw-graph-accent)"
            strokeDasharray={fact.trust == null ? '2 2' : undefined}
          />
        )}
      </svg>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-baseline gap-3">
          <span className="td-legend shrink-0">{role === 'hub' ? 'hub · most relations' : role}</span>
          <span className="td-value ml-auto min-w-0 truncate whitespace-nowrap text-xs" data-cell="numeric">
            {trustText(fact)} · {fact.degree} relations ·{' '}
            {fact.retrievals == null ? 'retrievals absent' : `${fact.retrievals} retrievals`}
          </span>
        </span>
        <span className={cn('truncate text-body', fact.restricted ? 'text-state-locked' : 'text-text-primary')}>
          {fact.label.split('\n')[0]}
        </span>
      </span>
    </div>
  );
}

/** Mark key: the brightness ramp, the dashed ring, the crosshatch, and in the
 * aggregate field the conflict cap on a disputed fact's tick. */
export function MarkKey({ disputedCap }: { disputedCap: boolean }) {
  return (
    <dl aria-label="Fact marks" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-3xs text-text-muted">
      <dt className="td-legend">trust</dt>
      <dd className="flex items-center gap-1.5">
        <svg aria-hidden width="60" height="8" viewBox="0 0 60 8">
          {[0, 0.25, 0.5, 0.75, 1].map((trust, index) => (
            <rect key={trust} x={index * 12} y={2} width={11} height={4} fill="var(--raw-graph-accent)" fillOpacity={trustOpacity(trust)} />
          ))}
        </svg>
        <span>0 → 1.00 · brighter higher</span>
      </dd>
      <dd className="flex items-center gap-1.5">
        <svg aria-hidden width="10" height="10" viewBox="0 0 10 10">
          <circle cx={5} cy={5} r={4} fill="none" stroke="var(--raw-graph-text)" strokeDasharray="2 2" />
        </svg>
        <span>dashed · trust absent</span>
      </dd>
      <dd className="flex items-center gap-1.5">
        <svg aria-hidden width="10" height="10" viewBox="0 0 10 10">
          <defs>
            <HatchDef id="key-hatch" />
          </defs>
          <rect x={1} y={1} width={8} height={8} fill="url(#key-hatch)" stroke="var(--raw-state-locked)" />
        </svg>
        <span>crosshatch · withheld</span>
      </dd>
      {disputedCap ? (
        <dd className="flex items-center gap-1.5">
          <svg aria-hidden width="6" height="12" viewBox="0 0 6 12">
            <rect x={1} y={0} width={4} height={3} fill="var(--raw-state-conflicting)" />
            <rect x={2} y={4} width={2} height={8} fill="var(--raw-graph-accent)" />
          </svg>
          <span>cap · disputed</span>
        </dd>
      ) : null}
    </dl>
  );
}

/** The relation kinds the field draws, with how many of each it draws. */
export function RelationKey({ relations }: { relations: readonly SceneRelation[] }) {
  const counts = new Map<ProjectMemoryGraphRelationKindV1, number>();
  for (const relation of relations) counts.set(relation.kind, (counts.get(relation.kind) ?? 0) + 1);
  const kinds = [...counts.keys()].sort((a, b) => a.localeCompare(b));
  return (
    <dl aria-label="Relations" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-3xs text-text-muted">
      <dt className="td-legend">relations</dt>
      {kinds.map((kind) => {
        const style = relationStroke(kind);
        return (
          <dd key={kind} className="flex items-center gap-1.5">
            <svg aria-hidden width="22" height="6" viewBox="0 0 22 6">
              <line x1="1" y1="3" x2="21" y2="3" stroke={style.stroke} strokeOpacity={style.opacity + 0.15} strokeWidth={style.width + 0.2} strokeDasharray={style.dash} />
            </svg>
            <span>
              {style.label} <span className="td-value text-text-secondary">{counts.get(kind)}</span>
            </span>
          </dd>
        );
      })}
    </dl>
  );
}

/** What this drawing covers, in the daemon's own accounting. */
export function CoverageFooter({
  coverage,
  graphRead,
  unplaced,
  children,
}: {
  coverage: SceneCoverage;
  graphRead: MemoryReadStatusV1 | undefined;
  unplaced: number;
  /** The drawing's keys, set on the same wrapping line as its accounting. */
  children?: ReactNode;
}) {
  const readState = graphRead?.state;
  const readComplete = readState === 'ready' || readState === 'complete_zero_findings';
  const parts: ReactNode[] = [
    <span key="roots">
      {coverage.drawnFacts.toLocaleString()} of {coverage.factUniverse.toLocaleString()} facts in the store drawn
    </span>,
    <span key="relations">
      {coverage.drawnRelations.toLocaleString()} of {coverage.relationCount.toLocaleString()} relations resolved, limit{' '}
      {coverage.relationLimit.toLocaleString()}
    </span>,
  ];
  const undrawnUnavailable = Math.max(0, coverage.unavailableFactCandidates - coverage.withheldDrawn);
  if (undrawnUnavailable > 0) {
    parts.push(
      <span key="unavailable" className="text-state-partial">
        {undrawnUnavailable.toLocaleString()} fact{' '}
        {undrawnUnavailable === 1 ? 'candidate' : 'candidates'} unavailable and not drawn
      </span>,
    );
  }
  if (coverage.danglingRelations > 0) {
    parts.push(
      <span key="dangling" className="text-state-partial">
        {coverage.danglingRelations.toLocaleString()}{' '}
        {coverage.danglingRelations === 1 ? 'relation names' : 'relations name'} a body this read did not include
      </span>,
    );
  }
  if (unplaced > 0) {
    parts.push(
      <span key="unplaced" className="text-state-partial">
        {unplaced.toLocaleString()} assertion or anchor {unplaced === 1 ? 'node' : 'nodes'} counted, not drawn
      </span>,
    );
  }
  return (
    <footer
      className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-edge-subtle/60 px-3 py-1 text-3xs text-text-muted"
      data-testid="fact-constellation-coverage"
    >
      {children}
      {readState && !readComplete ? (
        <StateChip kind={readState} detail={graphRead?.error ?? graphRead?.code ?? 'memory graph read'} />
      ) : null}
      <span className={cn('td-value', coverage.completeness !== 'complete' && 'text-state-partial')}>
        graph coverage {coverage.completeness}
        {coverage.omissionReasons.length > 0 ? `: ${coverage.omissionReasons.join(', ')}` : ''}
      </span>
      {parts.map((part, index) => (
        <span key={index} className="flex items-center gap-3">
          <span aria-hidden className="h-3 w-px bg-edge-subtle" />
          {part}
        </span>
      ))}
    </footer>
  );
}
