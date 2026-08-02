/** Explorer's read-out chrome: the per-lane readouts in the header, the
 * coordinator run panel, and the two filter-rail sections that explain what
 * answered and what did not. Every one of them renders a lane condition the
 * controller already decided. */
import { StateChip } from '../../ui/StateChip';
import { MetaLabel } from '../../ui/search/Highlight.tsx';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import type { ExplorerQueryRunV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { LANE_BY_ID, LANE_ICON } from './laneChrome.ts';
import {
  laneEvidence,
  laneHits,
  laneStateDetail,
  laneStateKind,
  runStateKind,
  sourceOutcomeStateKind,
  type ExplorerLaneReadModel,
} from './laneModel.ts';

/**
 * One memory's readout: what it is, how much of it we are holding, and how
 * well that quantity is known.
 *
 * The accessible name is composed entirely from visible text — there is no
 * `aria-label` override — so a screen reader and a sighted reader are told the
 * same sentence, and the visible label can never drift out of the name
 * (WCAG 2.5.3). The quantity is only ever a number the source actually
 * reported: a lane that did not answer shows its state and says so, never a
 * zero, and the proportional rail is drawn only when a real denominator
 * exists. Rows the source returned but the surface could not read are stated
 * rather than dropped, so the loaded count is never quietly short.
 */
export function LaneReadout({
  read,
  searching,
  active,
  onToggle,
}: {
  read: ExplorerLaneReadModel;
  searching: boolean;
  active: boolean;
  onToggle: () => void;
}) {
  const spec = LANE_BY_ID[read.lane];
  const Icon = LANE_ICON[read.lane];
  const loaded = laneHits(read).length;
  const total = read.state === 'ready' ? read.reportedTotal : null;
  const share = total != null && total > 0 ? loaded / total : null;
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onToggle}
      className={cn(
        'flex h-full w-full min-w-[7.5rem] flex-col gap-1.5 border px-2.5 py-2 text-left',
        'rounded-[var(--radius-standard)] transition-colors duration-[var(--dur-state)]',
        'ease-[var(--ease-standard)] motion-reduce:transition-none',
        active
          ? 'border-accent bg-surface-2'
          : 'border-edge-subtle bg-surface-0 hover:border-edge-strong',
      )}
    >
      <span className="flex items-center gap-1.5">
        <span
          aria-hidden
          className={cn('h-2.5 w-[3px] shrink-0 rounded-full', spec.railClass)}
        />
        {/* The coloured rail already carries lane identity, and at 320px the
          * icon plus its gap was the ~18px that pushed "Code graph" into an
          * ellipsis. The name outranks the glyph, so the glyph yields. */}
        <Icon aria-hidden size={12} className={cn('hidden shrink-0 sm:block', spec.textClass)} />
        <span className="td-legend truncate text-text-secondary">{spec.label}</span>
      </span>
      {read.state === 'ready' ? (
        <>
          <span className="flex flex-wrap items-baseline gap-x-1.5 gap-y-0.5">
            <span className="td-display text-base">{loaded.toLocaleString()}</span>
            <span className="min-w-0 text-2xs leading-tight text-text-muted">
              {searching
                ? total != null
                  ? `loaded of ${total.toLocaleString()} matching rows reported`
                  : 'loaded with no source total reported'
                : 'shown from the overview endpoint'}
            </span>
          </span>
          {read.unreadableRows > 0 ? (
            <span className="text-2xs leading-tight text-state-partial">
              {read.unreadableRows.toLocaleString()} of the rows returned could not be read
            </span>
          ) : null}
        </>
      ) : (
        <span className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5">
          <StateChip kind={laneStateKind(read)} detail={laneStateDetail(read)} />
          <span className="text-2xs leading-tight text-text-muted">no count reported</span>
        </span>
      )}
      {share !== null ? (
        <Meter fraction={share} className="w-full rounded-full" tone="bg-accent/80" />
      ) : null}
    </button>
  );
}

export function PlannerRunPanel({
  result,
  run,
  cancelling,
  onCancel,
}: {
  result: EnvelopeResult<ExplorerQueryRunV1> | undefined;
  run: ExplorerQueryRunV1 | undefined;
  cancelling: boolean;
  onCancel?: (() => void) | undefined;
}) {
  if (!run) {
    // One reading of the result, not two: the state and its detail were each
    // asking whether this was a transport failure, and the state arm carried a
    // ternary whose branches were the same value.
    const blocked =
      result?.outcome === 'transport'
        ? { kind: result.state, detail: result.detail ?? 'planner response unavailable' }
        : { kind: 'loading' as const, detail: 'admitting the source plan' };
    return (
      <section className="flex flex-col gap-2" aria-live="polite">
        <MetaLabel>Coordinator run</MetaLabel>
        <StateChip kind={blocked.kind} detail={blocked.detail} />
      </section>
    );
  }
  return (
    <section className="flex flex-col gap-2" aria-live="polite">
      <div className="flex flex-wrap items-center gap-2">
        <MetaLabel>Coordinator run</MetaLabel>
        <StateChip kind={runStateKind(run.state)} detail={run.finality} />
      </div>
      <p className="break-all font-mono text-2xs text-text-muted">{run.run_id}</p>
      <p className="text-2xs leading-relaxed text-text-secondary">{run.explanation}</p>
      {/* Revision and policy identifiers are single unbreakable tokens, and a
        * `1fr` track cannot shrink below an unbreakable word: in a 224px rail
        * both ran straight off the panel and read as `explorer-query-plan-`.
        * `min-w-0` lets the track shrink; `break-words` then wraps at the
        * hyphens and underscores the identifiers already contain, rather than
        * slicing mid-word the way `break-all` does. */}
      <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-2xs">
        <dt className="text-text-muted">Plan</dt>
        <dd className="min-w-0 break-words font-mono text-text-secondary">{run.plan_revision}</dd>
        <dt className="text-text-muted">Ordering</dt>
        <dd className="min-w-0 break-words text-text-secondary">{run.ordering_policy}</dd>
        <dt className="text-text-muted">Elapsed</dt>
        <dd className="td-value text-2xs">
          {Math.round(run.elapsed_micros / 1_000).toLocaleString()} ms
        </dd>
      </dl>
      <ul className="flex flex-col gap-1.5" aria-label="Source progress">
        {run.sources.map((source) => (
          <li
            key={source.source_id}
            className="flex min-w-0 flex-col gap-1 border-l-2 border-edge-strong pl-2"
          >
            <span className="flex flex-wrap items-center gap-1.5">
              <span className="text-2xs font-medium text-text-secondary">
                {source.source_label}
              </span>
              <StateChip kind={sourceOutcomeStateKind(source.outcome)} detail={source.phase} />
            </span>
            <span className="text-2xs text-text-muted">
              {source.completed_units !== null && source.total_units !== null
                ? `${source.completed_units.toLocaleString()} of ${source.total_units.toLocaleString()} ${source.coverage.unit ?? 'units'}`
                : source.completed_units !== null
                  ? `${source.completed_units.toLocaleString()} loaded · total unknown`
                  : 'denominator unknown'}
            </span>
            {source.message ? (
              <span className="text-2xs leading-relaxed text-text-muted">
                {source.error_code ? `${source.error_code}: ` : ''}
                {source.message}
              </span>
            ) : null}
          </li>
        ))}
      </ul>
      {onCancel ? (
        <button
          type="button"
          onClick={onCancel}
          disabled={cancelling}
          className="flex min-h-[var(--touch-target-min)] items-center justify-center border border-edge-subtle px-3 text-2xs text-text-secondary hover:border-accent hover:text-text-primary disabled:opacity-50"
        >
          {cancelling ? 'Requesting cancellation…' : 'Cancel this run'}
        </button>
      ) : null}
    </section>
  );
}

/** What each memory searches, and how well the quantity beside it is known. */
export function LaneLegend({
  lanes,
  searching,
}: {
  lanes: readonly ExplorerLaneReadModel[];
  searching: boolean;
}) {
  return (
    <section className="flex flex-col gap-2">
      <MetaLabel>What each lane searches</MetaLabel>
      <dl className="flex flex-col gap-2.5">
        {lanes.map((read) => {
          const spec = LANE_BY_ID[read.lane];
          const total = read.state === 'ready' ? read.reportedTotal : null;
          return (
            <div key={read.lane} className="flex flex-col gap-0.5">
              <dt className="flex items-center gap-2 text-2xs font-medium text-text-secondary">
                <span
                  aria-hidden
                  className={cn('h-3 w-[3px] shrink-0 rounded-full', spec.railClass)}
                />
                <span>{spec.label}</span>
              </dt>
              <dd className="flex flex-col gap-1 pl-[11px] text-2xs leading-relaxed text-text-muted">
                <span>
                  {searching ? spec.searches : spec.browseLabel}
                  {total != null ? ` · daemon reports ${total.toLocaleString()} matching` : ''}
                </span>
                {/* How well the quantity above is known, on the shared evidence
                  * axis: solid when the source reported a real denominator,
                  * hatched when rows arrived without one, dashed when the
                  * source never answered. */}
                <EvidencePattern quality={laneEvidence(read)} />
              </dd>
            </div>
          );
        })}
      </dl>
    </section>
  );
}

/** The lanes that neither answered nor are still reading, each named with the
 * condition that stopped it — an unavailable source, an unreachable daemon and
 * a refused read are different facts and read differently here. */
export function UnansweredLanes({ lanes }: { lanes: readonly ExplorerLaneReadModel[] }) {
  return (
    <section className="flex flex-col gap-1.5 border-l-2 border-state-partial pl-2">
      <MetaLabel>Unanswered</MetaLabel>
      {lanes.map((read) => (
        <p key={read.lane} className="flex flex-wrap items-center gap-1.5 text-2xs text-text-muted">
          <StateChip kind={laneStateKind(read)} detail={laneStateDetail(read)} />
          <span>{LANE_BY_ID[read.lane].label}</span>
        </p>
      ))}
      <p className="text-2xs leading-relaxed text-text-muted">
        Results are only from the lanes that answered. Nothing is being substituted for the rest,
        and no count is shown for a lane that reported none.
      </p>
    </section>
  );
}
