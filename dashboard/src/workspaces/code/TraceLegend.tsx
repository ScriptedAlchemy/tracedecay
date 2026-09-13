/**
 * The key below the field: the six panels that name what the picture encodes.
 *
 * `legendPanels` counts every figure on it from the model, so this module owns
 * only the marks — and each mark is the renderer's OWN geometry rather than a
 * drawing of it: the ribbon runs through `taperAt`, the sills through the same
 * proportions the renderer uses, and the hue chips through the same
 * `kindColorVars` the list rows do. That is what keeps a sample from drifting
 * away from the thing it claims to explain, and it is why the marks live here,
 * next to the panels they belong to, rather than in an icon set that no rule
 * connects to the field.
 */
import { legendPanels, type LegendSample } from '../../viz/trace/readout.ts';
import { taperAt } from '../../viz/trace/render.ts';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import type { TraceModel } from '../../viz/trace/types.ts';
import { TraceReading } from './TraceReading.tsx';

/** Points along the sample ribbon. Enough to read the curve, not a mesh. */
export const SAMPLE_STEPS = 24;

/** The legend's ribbon, drawn from the renderer's own taper profile. */
export function sampleRibbon(width: number, height: number): string {
  const mid = height / 2;
  const maxHalf = height / 2 - 0.5;
  const upper: string[] = [];
  const lower: string[] = [];
  for (let i = 0; i < SAMPLE_STEPS; i += 1) {
    const t = i / (SAMPLE_STEPS - 1);
    const x = (t * width).toFixed(2);
    const half = maxHalf * taperAt(t);
    upper.push(`${x},${(mid - half).toFixed(2)}`);
    lower.push(`${x},${(mid + half).toFixed(2)}`);
  }
  return `M${upper.join('L')}L${lower.reverse().join('L')}Z`;
}

/**
 * The mark beside a legend panel.
 *
 * Every one of these is the renderer's own geometry rather than a drawing of
 * it: the ribbon runs through `taperAt`, the sills through `sillWidth`, and
 * the hue chips carry the kinds actually on the field through the same
 * `kindColorVars` the list rows use. A sample that were merely *similar* to
 * the field would be one more thing that can drift.
 */
function SampleMark({ sample, model }: { sample: LegendSample; model: TraceModel }) {
  switch (sample) {
    case 'channel':
      return (
        <svg
          aria-hidden
          width={68}
          height={14}
          viewBox="0 0 68 14"
          className="shrink-0 text-text-secondary"
        >
          <path d={sampleRibbon(68, 14)} fill="currentColor" fillOpacity={0.32} />
          <path d={sampleRibbon(68, 14)} fill="none" stroke="currentColor" strokeWidth={0.7} />
        </svg>
      );
    case 'sill':
      return (
        <svg
          aria-hidden
          width={68}
          height={14}
          viewBox="0 0 68 14"
          className="shrink-0 text-text-secondary"
        >
          <rect x={2} y={4.5} width={18} height={5} rx={2.5} fill="currentColor" opacity={0.75} />
          <rect x={26} y={4.5} width={40} height={5} rx={2.5} fill="currentColor" opacity={0.75} />
        </svg>
      );
    case 'rows':
      return (
        <svg
          aria-hidden
          width={68}
          height={14}
          viewBox="0 0 68 14"
          className="shrink-0 text-text-secondary"
        >
          {[1.5, 7, 12.5].map((y) => (
            <line
              key={y}
              x1={2}
              y1={y}
              x2={66}
              y2={y}
              stroke="currentColor"
              strokeWidth={0.7}
              opacity={y === 7 ? 0.85 : 0.4}
            />
          ))}
          <circle cx={34} cy={7} r={2.6} fill="currentColor" />
        </svg>
      );
    case 'hue': {
      // The kinds actually drawn, in first-seen order. If the payload brings a
      // kind this app has never seen, it appears here automatically.
      const kinds = [...new Set(model.nodes.map((node) => node.kind))].slice(0, 5);
      return (
        <span aria-hidden className="flex h-3.5 shrink-0 items-center gap-1">
          {kinds.map((kind) => (
            <span
              key={kind}
              className="h-2.5 w-3 rounded-[2px] bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
              style={kindColorVars(kind)}
            />
          ))}
        </span>
      );
    }
    case 'membrane':
      return (
        <svg
          aria-hidden
          width={68}
          height={14}
          viewBox="0 0 68 14"
          className="shrink-0 text-text-secondary"
        >
          <rect
            x={1}
            y={1}
            width={66}
            height={12}
            rx={5}
            fill="currentColor"
            fillOpacity={0.09}
            stroke="currentColor"
            strokeWidth={0.7}
            strokeOpacity={0.7}
          />
          <rect x={9} y={5} width={16} height={4} rx={2} fill="currentColor" opacity={0.7} />
          <rect x={37} y={5} width={22} height={4} rx={2} fill="currentColor" opacity={0.7} />
        </svg>
      );
    case 'mouth':
      return (
        <svg
          aria-hidden
          width={68}
          height={14}
          viewBox="0 0 68 14"
          className="shrink-0 text-text-secondary"
        >
          {/* Full width to the mouth, then it stops — the absence beat the
            * design note asks every sheet to carry exactly once. */}
          <path d="M2 4.6 H40 V9.4 H2 Z" fill="currentColor" fillOpacity={0.32} />
          <path d="M2 4.6 H40 M2 9.4 H40" stroke="currentColor" strokeWidth={0.7} />
          <path
            d="M40 4.6 H64 M40 9.4 H64"
            stroke="currentColor"
            strokeWidth={0.7}
            strokeDasharray="3 3"
            strokeOpacity={0.75}
          />
        </svg>
      );
    default: {
      const exhaustive: never = sample;
      return exhaustive;
    }
  }
}

/**
 * The six-panel key below the field.
 *
 * The sheet's own sixth panel is sheet 01's module relief, dimmed behind the
 * flow. This surface draws no relief, so that slot goes to the sill — a
 * channel it does draw. See `legendPanels` for why that substitution is a
 * correctness requirement and not a preference.
 */
export function TraceLegend({ model }: { model: TraceModel }) {
  return (
    <dl className="grid grid-cols-1 gap-x-3 gap-y-2 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-6">
      {legendPanels(model).map((panel) => (
        <div key={panel.label} className="flex min-w-0 flex-col gap-1">
          <dt className="flex items-center gap-1.5">
            <span className="td-legend whitespace-normal">{panel.label}</span>
            <span aria-hidden className="td-rule" />
          </dt>
          <dd className="flex min-w-0 flex-col gap-1">
            <SampleMark sample={panel.sample} model={model} />
            <TraceReading value={panel.reading} size="text-2xs" />
            <span className="text-3xs leading-snug text-text-muted">{panel.teach}</span>
            {panel.qualifier === null ? null : (
              <span className="text-3xs leading-snug text-text-secondary">{panel.qualifier}</span>
            )}
          </dd>
        </div>
      ))}
    </dl>
  );
}
