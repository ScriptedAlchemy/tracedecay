/**
 * Marks, keys and the frame every constellation variant shares.
 *
 * Geometry is laid out in screen pixels from the measured container, so text
 * is set at its printed size (`LABEL_PX`) and never scales with the drawing.
 * A fact mark encodes trust as luminance of the one signal hue; a fact whose
 * trust the store did not report is a dashed hollow ring, and a withheld fact
 * is violet crosshatch whatever its other readings.
 */
import { useLayoutEffect, useRef, useState, type ReactNode } from 'react';

import type { MemoryReadStatusV1, ProjectMemoryGraphRelationKindV1 } from '../../../contracts/generated.ts';
import { Corners } from '../../../ui/instrument.tsx';
import { relationStyle } from '../constellation.ts';
import { CoverageFooter, TONE_OPACITY, TONE_STROKE, bodyOpacity } from '../FactConstellation.tsx';
import { accessWord, type FactScene, type SceneFact } from './factScene.ts';

export const LABEL_PX = 11;
export const LEGEND_PX = 10;

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
      if (rect.width > 0 && rect.height > 0) {
        setBox((current) =>
          Math.round(current.width) === Math.round(rect.width) &&
          Math.round(current.height) === Math.round(rect.height)
            ? current
            : { width: Math.round(rect.width), height: Math.round(rect.height) },
        );
      }
    };
    read();
    const observer = new ResizeObserver(read);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, box] as const;
}

export function relationStroke(kind: ProjectMemoryGraphRelationKindV1) {
  const style = relationStyle(kind);
  return {
    label: style.label,
    stroke: TONE_STROKE[style.tone],
    opacity: TONE_OPACITY[style.tone],
    width: style.tone === 'quiet' ? 0.9 : style.tone === 'conflict' ? 1.8 : 1.3,
    dash: style.dash,
    loud: style.tone === 'conflict' || style.tone === 'stale',
  };
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

/** One fact mark at a screen position. */
export function FactMark({
  fact,
  x,
  y,
  r,
  hatchId,
  hollowRing = false,
}: {
  fact: SceneFact;
  x: number;
  y: number;
  r: number;
  hatchId: string;
  /** Draw a measured fact as a solid ring rather than a disc (unconfirmed). */
  hollowRing?: boolean;
}) {
  if (fact.restricted) {
    return (
      <rect
        x={x - r}
        y={y - r}
        width={r * 2}
        height={r * 2}
        fill={`url(#${hatchId})`}
        stroke="var(--raw-state-locked)"
        strokeWidth={1}
        data-mark="restricted"
      />
    );
  }
  if (fact.trust == null) {
    return (
      <circle
        cx={x}
        cy={y}
        r={r}
        fill="none"
        stroke="var(--raw-graph-text)"
        strokeWidth={1.1}
        strokeDasharray="2 2"
        data-mark="trust-absent"
      />
    );
  }
  if (hollowRing) {
    return (
      <circle
        cx={x}
        cy={y}
        r={r}
        fill="none"
        stroke="var(--raw-graph-accent)"
        strokeOpacity={bodyOpacity(fact.trust)}
        strokeWidth={1.3}
        data-mark="unconfirmed"
      />
    );
  }
  return (
    <circle
      cx={x}
      cy={y}
      r={r}
      fill="var(--raw-graph-accent)"
      fillOpacity={bodyOpacity(fact.trust)}
      data-mark="measured"
    />
  );
}

/** Selection is a ring and inspection a raised halo; neither is glow alone. */
export function FocusMarks({ x, y, r, selected, inspected }: { x: number; y: number; r: number; selected: boolean; inspected: boolean }) {
  return (
    <>
      {inspected ? (
        <circle cx={x} cy={y} r={r + 6} fill="var(--raw-graph-accent)" fillOpacity={0.12} stroke="none" />
      ) : null}
      {selected ? (
        <circle cx={x} cy={y} r={r + 4} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={2} data-selected-ring />
      ) : null}
    </>
  );
}

/** Screen-pixel text with a surface halo so it stays legible over lines. */
export function Label({
  x,
  y,
  children,
  anchor = 'start',
  size = LABEL_PX,
  tone = 'var(--raw-graph-text)',
  opacity = 0.92,
  mono = true,
  weight,
}: {
  x: number;
  y: number;
  children: ReactNode;
  anchor?: 'start' | 'middle' | 'end';
  size?: number;
  tone?: string;
  opacity?: number;
  mono?: boolean;
  weight?: number;
}) {
  return (
    <text
      x={x}
      y={y}
      textAnchor={anchor}
      fontSize={size}
      fontFamily={mono ? 'var(--font-mono)' : 'var(--font-display)'}
      fontWeight={weight}
      fill={tone}
      opacity={opacity}
      stroke="var(--raw-surface-0)"
      strokeWidth={3}
      strokeLinejoin="round"
      paintOrder="stroke"
      className="pointer-events-none transition-opacity duration-[var(--dur-state)]"
    >
      {children}
    </text>
  );
}

export function trustText(fact: SceneFact): string {
  if (fact.trust != null) return `trust ${fact.trust.toFixed(2)}`;
  return fact.restricted ? `${accessWord(fact.access)} · trust absent` : 'trust absent';
}

/** The focal readout: the selected fact, else the inspected one, else the
 * most wired. It says which of the three it is. */
export function FocalReadout({
  fact,
  role,
  extra,
}: {
  fact: SceneFact | null;
  role: 'selected' | 'inspecting' | 'hub';
  extra?: ReactNode;
}) {
  if (!fact) return null;
  const roleText = role === 'hub' ? 'hub · most relations' : role;
  return (
    <div
      className="flex min-w-0 flex-1 items-center gap-2.5 border-l-2 border-accent bg-surface-0/70 py-1 pl-2 pr-3"
      data-testid="constellation-focal"
      data-focal-role={role}
    >
      <svg aria-hidden width="22" height="22" viewBox="0 0 22 22" className="shrink-0">
        <circle cx="11" cy="11" r="10" fill="none" stroke="var(--raw-graph-accent)" strokeOpacity={0.45} />
        <circle cx="11" cy="11" r="6.5" fill="var(--raw-graph-accent)" fillOpacity={fact.trust == null ? 0 : bodyOpacity(fact.trust)} stroke="var(--raw-graph-accent)" />
      </svg>
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="flex items-baseline justify-between gap-3">
          <span className="td-legend">{roleText}</span>
          <span className="font-mono text-2xs text-text-primary" data-cell="numeric">
            {trustText(fact)}
          </span>
        </span>
        <span className="truncate font-mono text-xs text-text-primary">{fact.label.split('\n')[0]}</span>
        <span className="truncate font-mono text-2xs text-text-secondary" data-cell="numeric">
          {fact.category ?? 'category absent'} · {fact.degree} relations ·{' '}
          {fact.retrievals == null ? 'retrievals absent' : `${fact.retrievals} retrievals`}
        </span>
      </span>
      {extra}
    </div>
  );
}

/** Mark key: luminance ramp, the two hollow forms and the crosshatch. */
export function MarkKey({ unconfirmed = false }: { unconfirmed?: boolean }) {
  return (
    <dl aria-label="Fact marks" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-3xs text-text-muted">
      <dt className="td-legend">marks</dt>
      <dd className="flex items-center gap-1.5">
        <svg aria-hidden width="46" height="10" viewBox="0 0 46 10">
          {[0.1, 0.4, 0.7, 1].map((trust, index) => (
            <circle key={trust} cx={5 + index * 12} cy={5} r={4} fill="var(--raw-graph-accent)" fillOpacity={bodyOpacity(trust)} />
          ))}
        </svg>
        <span>luminance · trust 0→1</span>
      </dd>
      {unconfirmed ? (
        <dd className="flex items-center gap-1.5">
          <svg aria-hidden width="10" height="10" viewBox="0 0 10 10">
            <circle cx={5} cy={5} r={4} fill="none" stroke="var(--raw-graph-accent)" strokeWidth={1.3} />
          </svg>
          <span>ring · no helpful feedback</span>
        </dd>
      ) : null}
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
    </dl>
  );
}

export function RelationKey({ scene }: { scene: FactScene }) {
  const counts = new Map<ProjectMemoryGraphRelationKindV1, number>();
  for (const relation of scene.relations) counts.set(relation.kind, (counts.get(relation.kind) ?? 0) + 1);
  const kinds = [...counts.keys()].sort((a, b) => a.localeCompare(b));
  if (kinds.length === 0) {
    return <p className="text-3xs text-text-muted">no fact-to-fact relation was returned in this read</p>;
  }
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

/** The figure every variant sits in: title, axes sentence, HUD, the drawing,
 * the keys, and the daemon's own coverage accounting. */
export function VariantFrame({
  variant,
  title,
  axes,
  scene,
  graphRead,
  focal,
  control,
  keys,
  children,
  description,
}: {
  variant: string;
  title: string;
  axes: string;
  scene: FactScene;
  graphRead: MemoryReadStatusV1 | undefined;
  /** The focal readout, set in the caption row. */
  focal: ReactNode;
  /** A camera control, set at the head of the key row. */
  control?: ReactNode;
  keys: ReactNode;
  children: ReactNode;
  description: string;
}) {
  return (
    <figure
      className="td-optic td-grain relative flex flex-col"
      data-testid="fact-constellation"
      data-constellation-variant={variant}
      aria-label={description}
    >
      <Corners tone="signal" />
      <figcaption className="relative z-10 flex flex-wrap items-start justify-between gap-2 px-3 pt-2">
        <span className="flex w-60 shrink-0 flex-col gap-1">
          <span className="td-title text-text-secondary">{title}</span>
          <span className="text-3xs tracking-[0.04em] text-text-muted">{axes}</span>
        </span>
        <span className="flex min-w-0 flex-1 basis-72 flex-wrap items-center gap-2">{focal}</span>
        <span className="flex items-baseline gap-3 bg-surface-0/60 px-2 py-1">
          <Hud label="facts drawn" value={scene.facts.length} />
          <Hud label="fact relations" value={scene.relations.length} />
          <Hud label="entities" value={scene.entities.length} />
        </span>
      </figcaption>
      <div className="relative z-10">{children}</div>
      <div className="relative z-10 flex flex-wrap items-center gap-x-6 gap-y-1.5 px-3 pb-2 pt-1">
        {control}
        {keys}
      </div>
      {scene.unplaced > 0 || scene.unplacedRelations > 0 ? (
        <p className="relative z-10 px-3 pb-1 text-3xs text-state-partial">
          {scene.unplaced} assertion or anchor {scene.unplaced === 1 ? 'node' : 'nodes'} and {scene.unplacedRelations}{' '}
          {scene.unplacedRelations === 1 ? 'relation' : 'relations'} to them are counted, not drawn here
        </p>
      ) : null}
      <CoverageFooter model={scene.model} graphRead={graphRead} />
    </figure>
  );
}

function Hud({ label, value }: { label: string; value: number }) {
  return (
    <span className="flex flex-col-reverse gap-0.5">
      <span className="td-legend">{label}</span>
      <span className="td-display text-base text-text-primary" data-cell="numeric">
        {value.toLocaleString()}
      </span>
    </span>
  );
}
