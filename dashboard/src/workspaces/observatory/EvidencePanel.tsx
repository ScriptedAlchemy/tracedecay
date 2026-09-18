import type { KeyboardEvent, ReactNode } from 'react';
import { cn } from '../../ui/cn.ts';
import { Corners } from '../../ui/instrument.tsx';
import {
  coveragePercent,
  evidenceStateLabel,
  evidenceTone,
  type EvidenceSourceId,
  type EvidenceState,
  type EvidenceSummary,
} from './evidence.ts';
import { formatMicrosUtc } from '../../ui/format.ts';

/** The element every panel's select button targets. One id, spelled once. */
export const EVIDENCE_INSPECTOR_ID = 'observatory-evidence-inspector';

/** The grade, printed: lamp beside word. The word is the carrier; the lamp is
 * redundant reinforcement and never the only signal. */
export function EvidenceChip({
  state,
  detail,
  size = 'legend',
  className,
}: {
  state: EvidenceState;
  detail?: string | null | undefined;
  size?: 'legend' | 'title';
  className?: string;
}) {
  const tone = evidenceTone(state);
  return (
    <span
      className={cn(
        'relative inline-flex max-w-full items-center gap-1.5 border border-edge-subtle bg-surface-2 pl-2 pr-1.5',
        size === 'title' ? 'py-1' : 'py-[3px]',
        className,
      )}
      data-evidence-state={state}
    >
      <span aria-hidden className={cn('absolute inset-y-0 left-0 w-[2px]', tone.lamp)} />
      <span className={cn(size === 'title' ? 'td-title' : 'td-legend', tone.ink)}>
        {evidenceStateLabel(state)}
      </span>
      {detail ? (
        <span className="min-w-0 truncate text-3xs tracking-[0.02em] text-text-muted">
          · {detail}
        </span>
      ) : null}
    </span>
  );
}

/** Coverage as the header prints it: a percent only when the authority gave
 * both figures; otherwise the completeness word or a stated absence. */
export function coverageLegend(summary: EvidenceSummary): string {
  const percent = coveragePercent(summary.coverage);
  if (percent != null) return `coverage ${percent}%`;
  if (summary.coverage) return `coverage ${summary.coverage.completeness}`;
  return 'coverage absent';
}

export function asOfLegend(summary: EvidenceSummary): string {
  if (summary.observedAtMicros == null) return 'observed. No time published';
  return `as of ${formatMicrosUtc(summary.observedAtMicros)}`;
}

/** Moves focus among the grid's panel buttons on arrow keys. Bound once on the
 * grid, so a panel added to the grid joins the traversal without wiring. */
export function panelRovingKeyDown(event: KeyboardEvent<HTMLElement>): void {
  const target = event.target;
  if (!(target instanceof HTMLElement) || !target.hasAttribute('data-evidence-panel-button')) {
    return;
  }
  const buttons = Array.from(
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[data-evidence-panel-button]'),
  );
  const index = buttons.indexOf(target as HTMLButtonElement);
  if (index < 0) return;
  let next: number;
  switch (event.key) {
    case 'ArrowRight':
    case 'ArrowDown':
      next = (index + 1) % buttons.length;
      break;
    case 'ArrowLeft':
    case 'ArrowUp':
      next = (index - 1 + buttons.length) % buttons.length;
      break;
    case 'Home':
      next = 0;
      break;
    case 'End':
      next = buttons.length - 1;
      break;
    default:
      return;
  }
  event.preventDefault();
  buttons[next]?.focus();
}

/**
 * One independently sourced panel of the overview.
 *
 * The whole face selects: the title button stretches an invisible hit area
 * over the panel, so clicking anywhere on it opens the inspector, while the
 * body stays plain read-out. Interactive rows a body wants of its own
 * (finding rows) sit above that overlay with `relative z-[1]`.
 *
 * Hover and focus preview the panel in the inspector; only click or Enter
 * selects it. Hover changes nothing but the inspector.
 */
export function EvidencePanel({
  summary,
  selected,
  previewed,
  onSelect,
  onPreview,
  onPreviewEnd,
  children,
  className,
}: {
  summary: EvidenceSummary;
  selected: boolean;
  previewed: boolean;
  onSelect: (id: EvidenceSourceId) => void;
  onPreview: (id: EvidenceSourceId) => void;
  onPreviewEnd: () => void;
  children: ReactNode;
  className?: string;
}) {
  const titleId = `evidence-panel-${summary.id}-title`;
  return (
    <section
      aria-labelledby={titleId}
      data-evidence-panel={summary.id}
      data-evidence-state={summary.state}
      data-evidence-selected={selected ? 'true' : undefined}
      data-evidence-previewed={previewed ? 'true' : undefined}
      className={cn(
        'relative flex min-w-0 flex-col border bg-surface-1',
        'transition-[background-color,border-color] duration-[var(--dur-state)]',
        'hover:bg-surface-2 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-1 has-[:focus-visible]:outline-accent',
        selected ? 'border-edge-strong td-raised' : 'border-edge-subtle',
        className,
      )}
      onPointerEnter={() => onPreview(summary.id)}
      onPointerLeave={onPreviewEnd}
    >
      <Corners tone={selected ? 'signal' : 'edge'} />
      {selected ? (
        <span aria-hidden className="absolute inset-y-0 left-0 w-[3px] bg-accent" />
      ) : null}
      <header className="flex h-8 shrink-0 items-center gap-2 border-b border-edge-subtle px-2.5">
        <button
          type="button"
          id={titleId}
          data-evidence-panel-button={summary.id}
          aria-pressed={selected}
          aria-controls={EVIDENCE_INSPECTOR_ID}
          className="td-title min-w-0 truncate text-left text-text-primary after:absolute after:inset-0 after:content-[''] focus-visible:outline-none"
          onClick={() => onSelect(summary.id)}
          onFocus={() => onPreview(summary.id)}
          onBlur={onPreviewEnd}
        >
          {summary.title}
        </button>
        <span aria-hidden className="td-rule" />
        <EvidenceChip state={summary.state} />
      </header>
      <div className="relative min-w-0 flex-1 p-2.5">{children}</div>
      <footer className="flex shrink-0 items-center justify-between gap-2 border-t border-edge-subtle px-2.5 py-1">
        <span className="td-legend truncate" data-evidence-as-of>
          {asOfLegend(summary)}
        </span>
        <span className="td-legend shrink-0" data-evidence-coverage>
          {coverageLegend(summary)}
        </span>
      </footer>
    </section>
  );
}

/** What a body shows when its read produced no payload: the daemon's word and
 * reason, and nothing drawn that could be mistaken for a measurement. */
export function BlockedBody({ summary }: { summary: EvidenceSummary }) {
  return (
    <p className="text-2xs leading-relaxed text-text-muted" data-evidence-blocked={summary.state}>
      <span className="text-text-secondary">{evidenceStateLabel(summary.state)}</span>
      {summary.stateDetail ? ` · ${summary.stateDetail}` : ''}
      {' · '}nothing is drawn in place of the reading
    </p>
  );
}
