import type { KeyboardEvent } from 'react';
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
} from './evidence.ts';

const LEGEND: readonly EvidenceState[] = ['measured', 'partial', 'stale', 'building', 'unavailable'];

const TICK_FRACTIONS = [0, 0.25, 0.5, 0.75, 1] as const;

function markRovingKeyDown(event: KeyboardEvent<HTMLElement>): void {
  const target = event.target;
  if (!(target instanceof HTMLElement) || !target.hasAttribute('data-timeline-mark')) return;
  const marks = Array.from(
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[data-timeline-mark]'),
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

/**
 * The shared time context: one rail, one mark per authority, placed at the
 * instant that authority says it observed what it reported.
 *
 * There is no wall clock here. The right edge is the newest published read,
 * labelled as such; a source that published no observation time is listed
 * beside the rail as a typed absence rather than placed at zero. No production
 * route accepts a time window, so the rail selects and previews but does not
 * filter — and says so.
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
  const model = timelineModel(summaries);
  const observations = summaries.find((summary) => summary.id === 'observations');
  const horizon =
    observatory.result?.outcome === 'envelope' ? observatory.result.envelope.payload.horizon : null;
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });
  const lanes = Math.max(0, ...model.marks.map((mark) => mark.lane)) + 1;
  const extent = model.extent;
  return (
    <section
      aria-label="Canonical observations timeline"
      data-observation-timeline
      data-timeline-marks={model.marks.length}
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
        className="td-optic td-grain relative mt-2 overflow-hidden"
        style={{ minHeight: 44 + Math.max(0, lanes - 1) * 10 }}
        data-timeline-extent={extent ? 'placed' : 'none'}
      >
        <Corners />
        <span aria-hidden className="absolute inset-x-3 top-1/2 h-px bg-edge-strong/70" />
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
                  className="td-legend absolute bottom-0.5 -translate-x-1/2 whitespace-nowrap text-[9px] text-text-muted"
                  style={{ left: `calc(12px + (100% - 24px) * ${fraction})` }}
                >
                  {label}
                </span>
              );
            })}
            <ul className="contents" aria-label="Observation marks">
              {model.marks.map((mark) => {
                const isSelected = selected === mark.id;
                const isPreviewed = previewed === mark.id;
                const tone = evidenceTone(mark.state);
                return (
                  <li key={mark.id} className="contents">
                    <button
                      type="button"
                      data-timeline-mark={mark.id}
                      data-evidence-state={mark.state}
                      aria-pressed={isSelected}
                      aria-controls={EVIDENCE_INSPECTOR_ID}
                      aria-label={`${mark.title} · ${evidenceStateLabel(mark.state)} · observed ${formatMicrosUtc(mark.observedAtMicros)}`}
                      title={`${mark.title} · ${formatMicrosUtc(mark.observedAtMicros)}`}
                      className="td-hit absolute -translate-x-1/2 -translate-y-1/2 focus-visible:outline-none"
                      style={{
                        left: `calc(12px + (100% - 24px) * ${mark.position ?? 1})`,
                        top: `calc(50% + ${mark.lane * 10}px)`,
                      }}
                      onClick={() => onSelect(mark.id)}
                      onPointerEnter={() => onPreview(mark.id)}
                      onPointerLeave={onPreviewEnd}
                      onFocus={() => onPreview(mark.id)}
                      onBlur={onPreviewEnd}
                    >
                      <span
                        aria-hidden
                        className={cn(
                          'block size-2 border border-surface-0 transition-transform duration-[var(--dur-state)]',
                          tone.lamp,
                          (isSelected || isPreviewed) && 'scale-150 outline outline-2 outline-offset-1 outline-accent',
                        )}
                      />
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
