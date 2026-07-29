/** Explorer's centre column: the result rows, the caption that says how much
 * of the index they speak for, and the empty states that refuse to overclaim
 * when they do not. */
import {
  DataRow,
  ListCaption,
  RESULT_ROW_HEIGHT,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { StateChip } from '../../ui/StateChip';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { Highlight } from '../../ui/search/Highlight.tsx';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import type { AbsenceVerdict } from './absence.ts';
import type { ExplorerFacet } from './controller.ts';
import { LANE_BY_ID, LANE_ICON } from './laneChrome.ts';
import { relativeTime, type Hit, type LaneId } from './model.ts';

export function ResultList({
  hits,
  terms,
  searching,
  pending,
  query,
  laneFilter,
  facet,
  unanswered,
  answeredLaneCount,
  laneCount,
  absence,
  selectedKey,
  onSelect,
  onClearFacet,
  onClearQuery,
}: {
  hits: Hit[];
  terms: readonly string[];
  searching: boolean;
  pending: boolean;
  query: string;
  laneFilter: LaneId | null;
  facet: ExplorerFacet | null;
  /** Whether any lane failed to answer, which forbids a zero-result claim. */
  unanswered: boolean;
  answeredLaneCount: number;
  laneCount: number;
  absence: AbsenceVerdict;
  selectedKey: string | undefined;
  onSelect: (hit: Hit) => void;
  onClearFacet: () => void;
  onClearQuery: () => void;
}) {
  if (hits.length === 0) {
    return (
      <EmptyResults
        searching={searching}
        pending={pending}
        query={query}
        facet={facet?.value ?? null}
        unanswered={unanswered}
        absence={absence}
        onClearFacet={onClearFacet}
        onClearQuery={onClearQuery}
      />
    );
  }
  return (
    <VirtualList
      items={hits}
      estimateHeight={RESULT_ROW_HEIGHT}
      getKey={(hit) => hit.key}
      header={
        <ListCaption>
          <span className="td-display text-xs">{hits.length.toLocaleString()}</span>
          <span>
            {searching ? 'results' : 'rows'}
            {laneFilter
              ? ` in ${LANE_BY_ID[laneFilter].label.toLowerCase()}`
              : // Never claim breadth the run did not deliver: a source that
                // failed, went unavailable, or is still reading did not
                // contribute to this set, and saying otherwise would credit it
                // for rows it never returned.
                answeredLaneCount === laneCount
                ? ` across ${laneCount} memories`
                : ` across ${answeredLaneCount} of ${laneCount} memories`}
            {facet ? ` · ${facet.value}` : ''}
          </span>
          <span aria-hidden className="ml-auto hidden sm:inline">
            ordered by each memory&rsquo;s own ranking
          </span>
        </ListCaption>
      }
      renderItem={(hit, index) => (
        <HitRow
          hit={hit}
          terms={terms}
          selected={selectedKey === hit.key}
          // A source-local order is only readable if the seam between two
          // sources is visible; without it seven rows from three independent
          // answers read as one ranked list.
          startsLane={index === 0 || hits[index - 1]?.lane !== hit.lane}
          onSelect={() => onSelect(hit)}
        />
      )}
    />
  );
}

function HitRow({
  hit,
  terms,
  selected,
  startsLane,
  onSelect,
}: {
  hit: Hit;
  terms: readonly string[];
  selected: boolean;
  /** First row of a run of rows from the same source. */
  startsLane: boolean;
  onSelect: () => void;
}) {
  const spec = LANE_BY_ID[hit.lane];
  const Icon = LANE_ICON[hit.lane];
  const age = relativeTime(hit.stamp);
  return (
    <DataRow
      selected={selected}
      onSelect={onSelect}
      height={RESULT_ROW_HEIGHT}
      railClassName={spec.railClass}
      className={cn('pl-4', startsLane && 'border-t border-edge-strong')}
    >
      <span className="flex w-10 shrink-0 flex-col items-center gap-0.5">
        <Icon aria-hidden size={13} className={cn(spec.textClass, 'opacity-80')} />
        <span className="td-value text-2xs leading-none text-text-muted">#{hit.rank}</span>
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-baseline gap-2">
          <Highlight
            text={hit.title}
            terms={terms}
            className={cn(
              'min-w-0 flex-1 truncate text-xs text-text-primary',
              hit.lane === 'code' && 'font-mono',
            )}
          />
          {hit.facet ? (
            <span className="shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs text-text-secondary">
              {hit.facet}
            </span>
          ) : null}
        </span>
        <span className="flex min-w-0 items-baseline gap-2 text-2xs text-text-muted">
          {hit.context ? (
            <Highlight
              text={hit.context}
              terms={terms}
              className="min-w-0 max-w-[22rem] shrink truncate"
            />
          ) : null}
          {hit.body ? (
            <Highlight text={hit.body} terms={terms} className="min-w-0 flex-1 truncate" />
          ) : null}
          {hit.matchedIn.length > 0 ? (
            <span className="hidden shrink-0 truncate text-text-secondary lg:inline">
              matched in {hit.matchedIn.join(', ')}
            </span>
          ) : null}
        </span>
      </span>
      {/* Absent measurements draw nothing at all. A row whose source omitted
        * the signal field has no bar and no number, rather than a rail at
        * zero that would read as a measured minimum. */}
      <span className="hidden w-28 shrink-0 items-center justify-end gap-2 md:flex">
        {hit.signal ? (
          <>
            <span className="tabular text-2xs text-text-muted">{hit.signal.display}</span>
            <Meter
              fraction={hit.signal.max > 0 ? hit.signal.value / hit.signal.max : null}
              className="w-10 rounded-full"
              tone="bg-accent/80"
              ariaLabel={`${hit.signal.field} ${hit.signal.value}`}
            />
          </>
        ) : null}
      </span>
      <span className="tabular w-10 shrink-0 text-right text-2xs text-text-muted">
        {age ?? ''}
      </span>
    </DataRow>
  );
}

function EmptyResults({
  searching,
  pending,
  query,
  facet,
  unanswered,
  absence,
  onClearFacet,
  onClearQuery,
}: {
  searching: boolean;
  pending: boolean;
  query: string;
  facet: string | null;
  unanswered: boolean;
  /** Whether a global-absence claim has been earned, and if not, the specific
   * thing standing in the way. */
  absence: AbsenceVerdict;
  onClearFacet: () => void;
  onClearQuery: () => void;
}) {
  if (pending) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <StateChip kind="loading" />
        <p className="text-2xs text-text-muted">The coordinator is reading required sources.</p>
      </div>
    );
  }
  if (facet) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <h2 className="text-sm font-semibold tracking-tight">
          Nothing loaded carries “{facet}”
        </h2>
        <p className="max-w-sm text-2xs leading-relaxed text-text-muted">
          The pivot is applied to the rows currently loaded, not to the whole index — a wider
          query may still contain this value.
        </p>
        <button
          type="button"
          onClick={onClearFacet}
          className="min-h-[var(--touch-target-min)] rounded-[var(--radius-chip)] border border-edge-subtle px-3 py-1 text-2xs text-text-secondary hover:border-accent hover:text-text-primary"
        >
          Clear the pivot
        </button>
      </div>
    );
  }
  if (unanswered) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <h2 className="text-sm font-semibold tracking-tight">
          Some sources did not answer
        </h2>
        <p className="max-w-md text-2xs leading-relaxed text-text-muted">
          The sources that answered returned no visible rows, but at least one source is
          unavailable. A zero-result claim would be unsafe, so Explorer keeps this result
          partial.
        </p>
      </div>
    );
  }
  if (searching) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <h2 className="text-sm font-semibold tracking-tight">
          {absence.confirmed ? `No source matched “${query}”` : `No rows loaded for “${query}”`}
        </h2>
        <p className="max-w-md text-2xs leading-relaxed text-text-muted">
          {absence.confirmed
            ? 'Every required source examined its full denominator with no unknown or omitted units, and the coordinator declared canonical finality.'
            : // The blocker in words, because "incomplete coverage" tells a
              // reader nothing they can act on, while "examined none of its 400
              // symbols" tells them to narrow the query.
              `${absence.reason}, so these bounded pages cannot establish global absence.`}
        </p>
        <EvidencePattern quality={absence.quality} />
        <button
          type="button"
          onClick={onClearQuery}
          className="min-h-[var(--touch-target-min)] rounded-[var(--radius-chip)] border border-edge-subtle px-3 py-1 text-2xs text-text-secondary hover:border-accent hover:text-text-primary"
        >
          Back to browsing
        </button>
      </div>
    );
  }
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
      <h2 className="text-sm font-semibold tracking-tight">Nothing to browse yet</h2>
      <p className="max-w-md text-2xs leading-relaxed text-text-muted">
        The overview endpoints answered with no rows, so there is nothing indexed to show.
        Index a project, or search directly for a term.
      </p>
    </div>
  );
}
