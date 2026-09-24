import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { useLiveActivity } from '../../data/sse/useEvents.tsx';
import { CenteredState } from '../../ui/ReadSection.tsx';
import { useEmergentPositions } from '../../viz/graph/fieldRenderers/emergentPositions.ts';
import { createPointField, unitsPerPoint } from '../../viz/graph/fieldRenderers/pointField.ts';
import { sampleFieldPalette, type FieldRenderer, type FieldScene } from '../../viz/graph/fieldRenderers/scene.ts';
import type { GraphCanvasEdge, GraphCanvasNode } from '../../viz/graph/types.ts';
import { useActivationField } from '../../viz/graph/useActivationField.ts';
import { useReducedMotion } from '../../viz/trace/reducedMotion.ts';
import { FIELD_HALF_LIFE_MS, buildGraphScene, strikeFor, traversalOrder } from './registryScene.ts';

/**
 * The Brain's measured point field and its host: renderer lifetime, the reader's
 * inspection and camera focus, keyboard traversal with a spoken reading, and
 * admitted activity. Hover and keyboard focus inspect; Enter selects through
 * the caller's production route; nothing here fires activity except an
 * admitted pulse naming a drawn body.
 */
export function BrainField({
  scene,
  inspectedId,
  onInspect,
  onSelect,
  focus,
  ariaLabel,
  legend,
  activity,
  className,
}: {
  scene: FieldScene;
  inspectedId: string | null;
  onInspect: (id: string | null) => void;
  onSelect: (id: string) => void;
  focus: ReadonlySet<string> | null;
  ariaLabel: string;
  legend: ReactNode;
  /** Whether admitted project activity reaches this field's bodies. */
  activity: boolean;
  className?: string;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const rendererRef = useRef<FieldRenderer | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const activation = useActivationField(FIELD_HALF_LIFE_MS);
  const { reduced } = useReducedMotion();
  const reducedRef = useRef(reduced);
  reducedRef.current = reduced;
  const inspectRef = useRef(onInspect);
  inspectRef.current = onInspect;
  const selectRef = useRef(onSelect);
  selectRef.current = onSelect;
  const viewRef = useRef({ inspected: inspectedId, focus });
  viewRef.current = { inspected: inspectedId, focus };

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let renderer: FieldRenderer;
    try {
      renderer = createPointField({
        container,
        scene,
        field: activation,
        palette: sampleFieldPalette(container),
        isReduced: () => reducedRef.current,
        onHover: (id) => inspectRef.current(id),
        onSelect: (id) => selectRef.current(id),
      });
    } catch (error) {
      setFailure(error instanceof Error ? error.message : 'The field renderer could not start.');
      return;
    }
    setFailure(null);
    rendererRef.current = renderer;
    renderer.setView(viewRef.current);
    // A container can lose its box for a frame while the page relayouts;
    // resizing into zero is what makes Sigma throw, so that frame is skipped.
    const resize =
      typeof ResizeObserver === 'function'
        ? new ResizeObserver(() => {
            if (container.clientWidth > 0 && container.clientHeight > 0) renderer.resize();
          })
        : null;
    resize?.observe(container);
    const theme = new MutationObserver(() => renderer.retheme(sampleFieldPalette(container)));
    theme.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
    return () => {
      resize?.disconnect();
      theme.disconnect();
      renderer.destroy();
      if (rendererRef.current === renderer) rendererRef.current = null;
    };
  }, [scene, activation]);

  useEffect(() => {
    rendererRef.current?.setView({ inspected: inspectedId, focus });
  }, [inspectedId, focus]);

  const { pulses, revision } = useLiveActivity();
  // Pulses already in the ring at mount are history, not activity to replay.
  const drawnRevision = useRef<number | null>(null);
  useEffect(() => {
    if (drawnRevision.current === null) {
      drawnRevision.current = revision;
      return;
    }
    if (!activity || revision === drawnRevision.current) return;
    const unseen = Math.min(revision - drawnRevision.current, pulses.length);
    drawnRevision.current = revision;
    for (const pulse of pulses.slice(pulses.length - unseen)) {
      const strike = strikeFor(pulse, scene);
      if (!strike) continue;
      rendererRef.current?.synapse({
        from: strike.touched,
        to: strike.hop[0] ?? null,
        at: performance.now(),
        label: strike.label,
        time: new Date(pulse.at).toLocaleTimeString([], { hour12: false }),
      });
      activation.strike([strike.touched], strike.energy);
      if (strike.hop.length > 0) activation.strike(strike.hop, strike.energy / 3);
    }
  }, [pulses, revision, activity, scene, activation]);

  const order = useMemo(() => traversalOrder(scene), [scene]);
  const inspectedBody = scene.bodies.find((body) => body.id === inspectedId);
  const keyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    const index = inspectedId == null ? -1 : order.indexOf(inspectedId);
    const step = (next: number): void => {
      event.preventDefault();
      const id = order[(next + order.length) % order.length];
      if (id) onInspect(id);
    };
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        step(index + 1);
        break;
      case 'ArrowLeft':
      case 'ArrowUp':
        step(index < 0 ? order.length - 1 : index - 1);
        break;
      case 'Home':
        step(0);
        break;
      case 'End':
        step(order.length - 1);
        break;
      case 'Enter':
      case ' ':
        if (inspectedBody?.role === 'body') {
          event.preventDefault();
          onSelect(inspectedBody.id);
        }
        break;
      case 'Escape':
        onInspect(null);
        break;
      case '+':
      case '=':
        rendererRef.current?.zoom(1.5);
        break;
      case '-':
        rendererRef.current?.zoom(1 / 1.5);
        break;
      default:
        break;
    }
  };

  if (failure) {
    return (
      <CenteredState
        title="The registry field could not draw"
        kind="unavailable"
        detail={`${failure} The project registry beside it lists the same projects.`}
      />
    );
  }
  return (
    <figure className={className ?? 'flex h-full min-h-0 flex-col gap-2'}>
      <div className="relative min-h-0 flex-1">
        <div
          ref={containerRef}
          data-brain-field
          tabIndex={0}
          role="group"
          aria-roledescription="field"
          aria-label={ariaLabel}
          aria-describedby="brain-field-keys"
          onKeyDown={keyDown}
          className="absolute inset-0 overflow-hidden border border-edge-subtle bg-[var(--raw-graph-substrate)] outline-offset-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        />
        <div
          role="group"
          aria-label="Field camera"
          className="absolute bottom-3 left-3 z-10 flex border border-edge-subtle bg-surface-0/90"
        >
          <button type="button" aria-label="Zoom out field" onClick={() => rendererRef.current?.zoom(1 / 1.5)} className="td-hit border-r border-edge-subtle px-3 text-xs text-text-secondary hover:bg-surface-2">−</button>
          <button type="button" aria-label="Zoom in field" onClick={() => rendererRef.current?.zoom(1.5)} className="td-hit border-r border-edge-subtle px-3 text-xs text-text-secondary hover:bg-surface-2">+</button>
          <button type="button" onClick={() => rendererRef.current?.fit()} className="td-hit px-3 text-2xs text-text-secondary hover:bg-surface-2">Fit</button>
        </div>
      </div>
      <p id="brain-field-keys" className="sr-only">
        Arrow keys step through projects, Enter selects project scope, Escape dismisses inspection, plus and
        minus zoom.
      </p>
      <p className="sr-only" aria-live="polite">
        {inspectedBody ? `${inspectedBody.label}: ${inspectedBody.detail.join(', ')}` : ''}
      </p>
      <figcaption className="text-2xs text-text-muted">{legend}</figcaption>
    </figure>
  );
}

/** The encoding the field draws, stated beside it. Amber is named as
 * admitted activity only; cyan as inspection only. */
export function FieldLegend({ scene }: { scene: FieldScene }) {
  const ratio = unitsPerPoint(scene);
  const registry = scene.columns != null;
  const encoding: ReadonlyArray<[string, string]> = registry
    ? [
        ['across', 'recency column (registry last seen)'],
        ['up', 'indexed mass, log'],
        ['point', ratio === 1 ? 'one indexed unit; stores ice at the core, artifacts by kind' : `${ratio} indexed units`],
        ['disc', 'area = indexed mass; brightness = recency'],
        ['line', 'exact shared git directory'],
        ...(scene.clusters.length > 0
          ? [['frame', 'a crowded recency × mass cell with its exact count; zoom or click to resolve'] as [string, string]]
          : []),
      ]
    : [
        ['point', 'one returned symbol; size = connectedness, hue = kind'],
        ['line', 'returned relation'],
      ];
  return (
    <span className="flex flex-wrap items-center gap-x-4 gap-y-1">
      {encoding.map(([label, value]) => <LegendItem key={label} label={label} value={value} />)}
      <span className="inline-flex items-center gap-1.5">
        <span aria-hidden className="h-2 w-3 bg-alert" />
        <span className="td-legend">amber</span>
        <span className="td-value text-3xs text-text-secondary">
          {registry
            ? 'admitted activity on the exact touched project and one drawn hop, 4.2 s half-life'
            : 'none: no symbol-level activity is supplied, so nothing here blooms'}
        </span>
      </span>
      <span className="inline-flex items-center gap-1.5">
        <span aria-hidden className="size-2.5 rounded-full border-2 border-accent" />
        <span className="td-legend">cyan</span>
        <span className="td-value text-3xs text-text-secondary">inspection and focus, never activity</span>
      </span>
    </span>
  );
}

/** A project's returned symbol graph on the same field. Symbols carry no
 * admitted activity, so this field never blooms. */
export function ScopedField({
  nodes,
  edges,
  inspectedId,
  onInspect,
  label,
  caption,
}: {
  nodes: readonly GraphCanvasNode[];
  edges: readonly GraphCanvasEdge[];
  inspectedId: string | null;
  onInspect: (id: string | null) => void;
  label: string;
  /** What the returned slice is, including any daemon cap. */
  caption: ReactNode;
}) {
  const layout = useEmergentPositions(nodes, edges);
  const scene = useMemo(() => {
    if (layout.state !== 'ready') return null;
    return buildGraphScene(
      nodes.map((node) => {
        const [x, y] = layout.positions.get(node.id) ?? [0, 0];
        return { ...node, x, y };
      }),
      edges,
    );
  }, [nodes, edges, layout]);
  if (layout.state === 'failed') {
    return (
      <CenteredState
        title="The force layout could not be completed"
        kind="unavailable"
        detail={`${layout.reason}. The returned symbol list remains available.`}
      />
    );
  }
  if (!scene) {
    return <p role="status" data-state="loading" className="p-3 text-2xs text-text-secondary">Calculating graph positions. The symbol list remains available.</p>;
  }
  return (
    <BrainField
      scene={scene}
      inspectedId={inspectedId}
      onInspect={onInspect}
      onSelect={onInspect}
      focus={null}
      activity={false}
      className="flex h-full min-h-[58vh] flex-col gap-2 lg:min-h-0"
      ariaLabel={`${label} code graph: ${nodes.length} returned symbols, ${edges.length} returned relations. The returned symbol list alongside is the exact equivalent.`}
      legend={
        <span className="flex flex-col gap-1">
          <FieldLegend scene={scene} />
          <span>{caption}</span>
        </span>
      }
    />
  );
}

function LegendItem({ label, value }: { label: string; value: string }) {
  return (
    <span className="inline-flex items-baseline gap-1.5">
      <span className="td-legend">{label}</span>
      <span className="td-value text-3xs text-text-secondary">{value}</span>
    </span>
  );
}
