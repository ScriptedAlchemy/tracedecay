import { useLayoutEffect, useRef, useState, type KeyboardEvent, type RefObject } from 'react';
import type { ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { cn } from '../../ui/cn.ts';
import { Corners } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { EVIDENCE_INSPECTOR_ID, EvidenceChip } from './EvidencePanel.tsx';
import {
  evidenceStateLabel,
  evidenceTone,
  relativeTickLabel,
  timelineModel,
  type EvidenceRead,
  type EvidenceSourceId,
  type EvidenceState,
  type EvidenceSummary,
  type TimelineCluster,
} from './evidence.ts';

const LEGEND: readonly EvidenceState[] = ['measured', 'partial', 'stale', 'building', 'unavailable'];

const TICK_FRACTIONS = [0, 0.25, 0.5, 0.75, 1] as const;

/** The rail is inset this far from the well's edges so an end mark's hit area
 * stays inside the well. */
const RAIL_INSET_PX = 24;

/** Two marks closer than one touch target cannot both be hit; they cluster. */
const CLUSTER_GAP_PX = 44;

function markRovingKeyDown(event: KeyboardEvent<HTMLElement>): void {
  const target = event.target;
  if (!(target instanceof HTMLElement) || !target.hasAttribute('data-timeline-cluster')) return;
  const marks = Array.from(
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[data-timeline-cluster]'),
  );
  const index = marks.indexOf(target as HTMLButtonElement);
  if (index < 0) return;
  let next: number;
  switch (event.key) {
    case 'ArrowRight':
    case 'ArrowDown':
      next = (index + 1) % marks.length;
      break;
    case 'ArrowLeft':
    case 'ArrowUp':
      next = (index - 1 + marks.length) % marks.length;
      break;
    case 'Home':
      next = 0;
      break;
    case 'End':
      next = marks.length - 1;
      break;
    default:
      return;
  }
  event.preventDefault();
  marks[next]?.focus();
}

/** The rail's usable width in CSS pixels, so the cluster threshold is one
 * touch target however wide the well is. Falls back to a whole-rail guess
 * until the first layout. */
function useRailWidth(): [RefObject<HTMLDivElement | null>, number] {
  const rail = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(1200);
  useLayoutEffect(() => {
    const node = rail.current;
    if (!node) return;
    const measure = () => setWidth(Math.max(1, node.clientWidth - RAIL_INSET_PX * 2));
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  return [rail, width];
}

/**
 * The shared time context: one rail, one mark per authority, placed at the
 * instant that authority says it observed what it reported.
 *
 * There is no wall clock here. The right edge is the newest published read,
 * labelled as such; a source that published no observation time is listed
 * beside the rail as a typed absence rather than placed at zero. No production
 * route accepts a time window, so the rail selects and previews but does not
 * filter, and says so. Reads that land within one touch target of each other
 * fold into a cluster that states its count and opens into its members.
 */
export function ObservationTimeline({
  summaries,
  observatory,
  selected,
  previewed,
  onSelect,
  onPreview,
  onPreviewEnd,
}: {
  summaries: readonly EvidenceSummary[];
  observatory: EvidenceRead<ObservatoryReadModelV1>;
  selected: EvidenceSourceId | null;
  previewed: EvidenceSourceId | null;
  onSelect: (id: EvidenceSourceId) => void;
  onPreview: (id: EvidenceSourceId) => void;
  onPreviewEnd: () => void;
}) {
  const [rail, railWidth] = useRailWidth();
  const model = timelineModel(summaries, CLUSTER_GAP_PX / railWidth);
  const [openCluster, setOpenCluster] = useState<string | null>(null);
  const observations = summaries.find((summary) => summary.id === 'observations');
  const horizon =
    observatory.result?.outcome === 'envelope' ? observatory.result.envelope.payload.horizon : null;
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });
  const extent = model.extent;
  const opened = model.clusters.find((cluster) => cluster.key === openCluster && cluster.marks.length > 1);
  const railLeft = (position: number) =>
    `calc(${RAIL_INSET_PX}px + (100% - ${RAIL_INSET_PX * 2}px) * ${position})`;

  const activateCluster = (cluster: TimelineCluster) => {
    const only = cluster.marks[0];
    if (cluster.marks.length === 1 && only) {
      onSelect(only.id);
      return;
    }
    setOpenCluster((current) => (current === cluster.key ? null : cluster.key));
  };

  return (
    <section
      aria-label="Canonical observations timeline"
      data-observation-timeline
      data-timeline-marks={model.marks.length}
      data-timeline-clusters={model.clusters.length}
      className="relative border-b border-edge-subtle bg-surface-1 px-3 py-2"
      onKeyDown={markRovingKeyDown}
    >
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <button
          type="button"
          data-evidence-panel-button="observations"
          aria-pressed={selected === 'observations'}
          aria-controls={EVIDENCE_INSPECTOR_ID}
          className={cn(
            'td-hit -my-2 border-b border-transparent px-1 text-left',
            selected === 'observations' && 'border-accent',
          )}
          onClick={() => onSelect('observations')}
          onPointerEnter={() => onPreview('observations')}
          onPointerLeave={onPreviewEnd}
          onFocus={() => onPreview('observations')}
          onBlur={onPreviewEnd}
        >
          <span className="td-title text-text-primary">Canonical observations timeline</span>
        </button>
        {observations ? <EvidenceChip state={observations.state} detail={observations.stateDetail} /> : null}
        <span className="td-legend min-w-0 truncate">
          {horizon
            ? `horizon ${stamp(horizon.since_micros)} → ${stamp(horizon.until_micros)}`
            : 'horizon not published'}
        </span>
        <span aria-hidden className="td-rule max-sm:hidden" />
        <ul className="flex flex-wrap items-center gap-x-3 gap-y-1" aria-label="Timeline legend">
          {LEGEND.map((state) => (
            <li key={state} className="td-legend flex items-center gap-1.5">
              <span aria-hidden className={cn('size-1.5 shrink-0', evidenceTone(state).lamp)} />
              {evidenceStateLabel(state)}
            </li>
          ))}
        </ul>
      </div>

      <div
        ref={rail}
        className="td-optic td-grain relative mt-2 h-14 overflow-hidden"
        data-timeline-extent={extent ? 'placed' : 'none'}
      >
        <Corners />
        <span
          aria-hidden
          className="absolute top-1/2 h-px bg-edge-strong/70"
          style={{ left: RAIL_INSET_PX, right: RAIL_INSET_PX }}
        />
        {extent ? (
          <>
            {TICK_FRACTIONS.map((fraction) => {
              const at = extent.oldestMicros + (extent.newestMicros - extent.oldestMicros) * fraction;
              const label =
                fraction === 1 ? 'newest read' : relativeTickLabel(extent.newestMicros - at);
              if (extent.newestMicros === extent.oldestMicros && fraction !== 1) return null;
              return (
                <span
                  key={fraction}
                  aria-hidden
                  className={cn(
                    'td-legend absolute bottom-0.5 whitespace-nowrap text-[9px] text-text-muted',
                    fraction === 0 ? undefined : fraction === 1 ? '-translate-x-full' : '-translate-x-1/2',
                  )}
                  style={{ left: railLeft(fraction) }}
                >
                  {label}
                </span>
              );
            })}
            <ul className="contents" aria-label="Observation marks">
              {model.clusters.map((cluster) => {
                const single = cluster.marks.length === 1 ? cluster.marks[0] : null;
                const holdsSelected = cluster.marks.some((mark) => mark.id === selected);
                const holdsPreviewed = cluster.marks.some((mark) => mark.id === previewed);
                const lit = holdsSelected || holdsPreviewed || openCluster === cluster.key;
                const label = single
                  ? `${single.title} · ${evidenceStateLabel(single.state)} · observed ${formatMicrosUtc(single.observedAtMicros)}`
                  : `${cluster.marks.length} reads between ${formatMicrosUtc(cluster.oldestMicros)} and ${formatMicrosUtc(cluster.newestMicros)} · open to choose one`;
                return (
                  <li key={cluster.key} className="contents">
                    <button
                      type="button"
                      data-timeline-cluster={cluster.key}
                      data-timeline-mark={single?.id}
                      data-timeline-cluster-size={cluster.marks.length}
                      data-evidence-state={single?.state}
                      aria-pressed={single ? holdsSelected : openCluster === cluster.key}
                      aria-expanded={single ? undefined : openCluster === cluster.key}
                      aria-controls={single ? EVIDENCE_INSPECTOR_ID : `timeline-cluster-${cluster.key}`}
                      aria-label={label}
                      title={label}
                      className="td-hit absolute -translate-x-1/2 -translate-y-1/2 focus-visible:outline-none"
                      style={{ left: railLeft(cluster.position), top: '50%' }}
                      onClick={() => activateCluster(cluster)}
                      onPointerEnter={single ? () => onPreview(single.id) : undefined}
                      onPointerLeave={single ? onPreviewEnd : undefined}
                      onFocus={single ? () => onPreview(single.id) : undefined}
                      onBlur={single ? onPreviewEnd : undefined}
                    >
                      <span
                        aria-hidden
                        className={cn(
                          'flex flex-col items-center gap-0.5 transition-transform duration-[var(--dur-state)]',
                          lit && 'scale-110 outline outline-2 outline-offset-2 outline-accent',
                        )}
                      >
                        {single ? null : (
                          <span className="td-legend text-[9px] leading-none text-text-primary">
                            {cluster.marks.length}
                          </span>
                        )}
                        <span className="flex items-center gap-px">
                          {cluster.marks.slice(0, 4).map((mark) => (
                            <span
                              key={mark.id}
                              className={cn(
                                'block border border-surface-0',
                                single ? 'size-2' : 'size-1.5',
                                evidenceTone(mark.state).lamp,
                              )}
                            />
                          ))}
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </>
        ) : (
          <p className="td-legend absolute inset-0 flex items-center justify-center text-text-muted">
            no authority has published an observation time · nothing is placed
          </p>
        )}
      </div>

      {opened ? (
        <ul
          id={`timeline-cluster-${opened.key}`}
          aria-label={`Reads in the opened cluster`}
          data-timeline-cluster-open={opened.key}
          className="mt-1.5 grid gap-x-3 sm:grid-cols-2 xl:grid-cols-3"
        >
          {opened.marks.map((mark) => {
            const isSelected = selected === mark.id;
            return (
              <li key={mark.id}>
                <button
                  type="button"
                  data-timeline-mark={mark.id}
                  data-evidence-state={mark.state}
                  aria-pressed={isSelected}
                  aria-controls={EVIDENCE_INSPECTOR_ID}
                  className={cn(
                    'td-hit flex w-full items-center gap-2 border px-2 text-left text-2xs',
                    isSelected
                      ? 'border-edge-strong bg-surface-3'
                      : 'border-transparent hover:border-edge-subtle hover:bg-surface-2',
                  )}
                  onClick={() => onSelect(mark.id)}
                  onPointerEnter={() => onPreview(mark.id)}
                  onPointerLeave={onPreviewEnd}
                  onFocus={() => onPreview(mark.id)}
                  onBlur={onPreviewEnd}
                >
                  <span aria-hidden className={cn('size-1.5 shrink-0', evidenceTone(mark.state).lamp)} />
                  <span className="min-w-0 flex-1 truncate text-text-primary">{mark.title}</span>
                  <span className="td-legend shrink-0">{evidenceStateLabel(mark.state)}</span>
                  <span className="td-legend shrink-0 tabular max-sm:hidden">
                    {formatMicrosUtc(mark.observedAtMicros)}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      ) : null}

      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-3xs text-text-muted">
        <span data-timeline-newest>
          {extent
            ? `newest read ${formatMicrosUtc(extent.newestMicros)} · oldest ${formatMicrosUtc(extent.oldestMicros)}`
            : 'no placed reads'}
        </span>
        <span data-timeline-filter="unavailable">
          range filter unavailable · no authority accepts a time window; marks select and preview only
        </span>
        {model.unplaced.length > 0 ? (
          <span data-timeline-unplaced={model.unplaced.length}>
            no observation time published:{' '}
            {model.unplaced.map((entry) => entry.title.toLowerCase()).join(', ')}
          </span>
        ) : null}
      </div>
    </section>
  );
}
