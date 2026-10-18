import { useEffect, useRef, useState, type CSSProperties, type RefObject } from 'react';
import type { DeliveryAttentionSourceV1 } from '../../contracts/generated.ts';
import { GradeMark } from './deliveryChrome.tsx';
import { EVIDENCE_GRADES } from './evidence.ts';
import { attentionCode } from './rendererModel.ts';

/**
 * Marks the Delivery lanes field and journey transit share. Amber appears in exactly one
 * place, the attention beacon, and every field that draws one also mounts
 * `AttentionLegend`, which names that meaning in text.
 */

/** The element's CSS size; `fallback` where `ResizeObserver` is absent (jsdom). */
export function useMeasuredSize(fallback: {
  width: number;
  height: number;
}): [RefObject<HTMLDivElement | null>, { width: number; height: number }] {
  const ref = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState<{ width: number; height: number } | null>(null);
  useEffect(() => {
    const element = ref.current;
    if (element === null || typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect !== undefined && rect.width > 0) setSize({ width: rect.width, height: rect.height });
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, size ?? fallback];
}

/** One active attention item: an amber triangle and its engraved source code. */
export function AttentionBeacon({
  x,
  y,
  source,
  showCode = true,
}: {
  x: number;
  y: number;
  source: DeliveryAttentionSourceV1;
  showCode?: boolean;
}) {
  return (
    <g data-beacon={source}>
      <path d={`M ${x} ${y - 4} L ${x + 4} ${y + 3} L ${x - 4} ${y + 3} Z`} fill="var(--raw-graph-alert)" />
      {showCode ? (
        <text
          x={x + 6}
          y={y + 3}
          fontSize="8.5"
          fontFamily="var(--font-mono)"
          letterSpacing="0.06em"
          fill="var(--raw-graph-alert)"
        >
          {attentionCode(source)}
        </text>
      ) : null}
    </g>
  );
}

/** Attention the daemon could not evaluate: a gray dashed ring, never amber. */
export function UnevaluatedGlyph({ x, y }: { x: number; y: number }) {
  return (
    <circle cx={x} cy={y} r={3.5} fill="none" stroke="var(--raw-graph-text)" strokeOpacity="0.6" strokeDasharray="1.5 1.5" />
  );
}

/** Diagonal hatch for NO EVIDENCE bands, shared by SVG fields. */
export function HatchDefs({ id }: { id: string }) {
  return (
    <pattern id={id} width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
      <line x1="0" y1="0" x2="0" y2="6" stroke="var(--raw-graph-edge)" strokeWidth="1.2" strokeOpacity="0.55" />
    </pattern>
  );
}

/** The same hatch for DOM plates. */
export const HATCH_STYLE: CSSProperties = {
  backgroundImage:
    'repeating-linear-gradient(135deg, color-mix(in oklab, var(--raw-graph-edge) 45%, transparent) 0 1px, transparent 1px 6px)',
};

/** Names amber and every attention code the field actually draws. */
export function AttentionLegend({
  sources,
  unevaluated,
}: {
  sources: readonly DeliveryAttentionSourceV1[];
  unevaluated: number;
}) {
  const present = [...new Set(sources)].sort();
  return (
    <ul aria-label="Attention legend" className="flex flex-wrap items-center gap-x-4 gap-y-1 text-3xs text-text-muted">
      <li className="flex items-center gap-1.5 text-alert">
        <svg aria-hidden width="10" height="10" viewBox="-5 -5 10 10">
          <path d="M 0 -4 L 4 3 L -4 3 Z" fill="currentColor" />
        </svg>
        amber = active attention, named source
      </li>
      {present.length === 0 ? (
        <li>no active attention served</li>
      ) : (
        present.map((source) => (
          <li key={source} className="text-text-secondary">
            <span className="td-value">{attentionCode(source)}</span> = {source.replaceAll('_', ' ')}
          </li>
        ))
      )}
      {unevaluated > 0 ? (
        <li className="flex items-center gap-1.5">
          <svg aria-hidden width="10" height="10" viewBox="-5 -5 10 10">
            <circle r="3.5" fill="none" stroke="currentColor" strokeDasharray="1.5 1.5" />
          </svg>
          {unevaluated} attention source{unevaluated === 1 ? '' : 's'} not evaluated
        </li>
      ) : null}
    </ul>
  );
}

/** The ladder's full stroke grammar, printed once per field. */
export function GradeLegend() {
  return (
    <span aria-label="Evidence grade legend" className="flex flex-wrap items-center gap-x-3 gap-y-1">
      {EVIDENCE_GRADES.map((grade) => (
        <GradeMark key={grade} grade={grade} />
      ))}
    </span>
  );
}
