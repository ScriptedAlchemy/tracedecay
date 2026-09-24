import { useCallback, useEffect, useRef, type ReactNode } from 'react';
import { useSearchParams } from 'react-router';

import { WorkspaceHeader } from '../../ui/instrument.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { cn } from '../../ui/cn';
import { useExplorerController, type ExplorerController } from './controller.ts';
import { HitInspector, type InspectMode } from './Inspector.tsx';
import { Lane, LaneGrid } from './Lane.tsx';
import { LANE_BY_ID } from './laneChrome.ts';
import { laneHits } from './laneModel.ts';
import { facetCounts, LANES, SOURCE_LANE_IDS, type Hit, type LaneId } from './model.ts';
import { RunRegister } from './panels.tsx';

/**
 * Explorer, one query, four independent authorities.
 *
 * TraceDecay remembers a repository several separate ways, and the honest
 * consequence is that a search is a *fan-out*, not a single ranked list: the
 * code graph, the transcript store and the fact store each answer for
 * themselves, and the semantic lane the product contract names has no
 * registered authority at all. This surface makes that structure the design.
 * Each authority is a lane with its own lifecycle, count, rows and typed
 * absence; a ready lane can never make its neighbour look ready; and because
 * the daemon returns hits but no relevance score, "why this is here" is told
 * with things that are true, the source's own order, the fields whose text
 * really contains the term, and measured quantities that name their field.
 *
 * Before a query, the lanes browse what each memory holds right now, from
 * the same endpoints' overview shapes.
 *
 * Hover inspects; click selects. The page composes: `controller.ts` owns the
 * run and the query state, `laneModel.ts` turns the wire into one typed
 * condition per lane, `Lane.tsx` draws a condition, `Inspector.tsx` grades a
 * result.
 */
export function ExplorerPage() {
  const explorer = useExplorerController();
  useExplorerUrlState(explorer);
  const { select, peek, selected, peeked } = explorer;

  // Escape closes the selection and hands focus back to the row that opened
  // it. Bound on the document, because focus while reading the inspector
  // usually sits on its scroll region, not on a control the page owns. A
  // `defaultPrevented` Escape is the search field clearing itself, which must
  // not also close the panel. Escape never cancels a run: cancellation is the
  // explicit control in the run register.
  const invokerRef = useRef<HTMLElement | null>(null);
  // Focus landing on a row inspects it, so handing focus back to the row that
  // opened the inspector would reopen it as a peek in the same keystroke that
  // closed it. The restore is flagged so that one focus event is not a peek;
  // the next arrow key inspects again as usual.
  const restoringFocus = useRef(false);
  const closeInspector = useCallback(() => {
    select(null);
    peek(null);
    restoringFocus.current = true;
    invokerRef.current?.focus();
    restoringFocus.current = false;
    invokerRef.current = null;
  }, [peek, select]);
  const onPeek = useCallback(
    (hit: Hit | null) => {
      if (restoringFocus.current) return;
      peek(hit);
    },
    [peek],
  );
  useEffect(() => {
    if (selected === null) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      event.stopPropagation();
      closeInspector();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [closeInspector, selected]);

  // The inspector shows the row under inspection first, the selection
  // otherwise. A peek is transient and fetches nothing; the selection
  // persists. Resting on the selected row itself is not a peek, the row a
  // keyboard reader just selected still has focus, and it is selected.
  const inspected = peeked ?? selected;
  const mode: InspectMode =
    peeked !== null && peeked.key !== selected?.key ? 'peek' : 'selected';

  const laneFilterValue = explorer.laneFilter ?? 'all';
  const filtersActive = explorer.laneFilter !== null || explorer.facet !== null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <WorkspaceHeader
        path="explorer"
        title="Explorer"
        note="one query · four independent authorities · no cross-lane rank"
      />

      {/* The query register: the field, the run's own state, and the lane and
        * pivot controls. Every control here is real, it changes what is
        * asked or what loaded rows are shown, and none changes project scope,
        * which the shell register owns. */}
      <div className="flex shrink-0 flex-col gap-2 border-b border-edge-subtle bg-surface-1 px-3 py-2">
        <div className="flex flex-col gap-2 lg:flex-row lg:items-start lg:gap-4">
          <SearchField
            value={explorer.query}
            onChange={explorer.setQuery}
            onSubmit={() => explorer.submit()}
            onClear={explorer.reset}
            submitted={explorer.submitted}
            label="Query code, sessions, and knowledge"
            placeholder="Query everything the daemon remembers…"
            hint={
              explorer.searching ? (
                <>
                  hits for{' '}
                  <span className="font-medium text-text-secondary">"{explorer.submitted}"</span>{' '}
                  in each source&rsquo;s own order · terms are marked where they occur in the
                  payload
                </>
              ) : (
                <>
                  browsing what each authority holds · quote a phrase to keep it whole · press{' '}
                  <kbd className="border border-edge-subtle px-1">/</kbd> to focus,{' '}
                  <kbd className="border border-edge-subtle px-1">Esc</kbd> to return to browsing
                </>
              )
            }
          />
          {explorer.searching ? (
            <RunRegister
              result={explorer.runResult}
              run={explorer.run}
              writability={explorer.writability}
              progress={explorer.runProgress}
              cancelling={explorer.cancelling}
              onCancel={explorer.cancel}
              className="lg:min-h-[calc(var(--touch-target-min)+2px)] lg:max-w-[40%]"
            />
          ) : null}
        </div>

        <div className="flex flex-wrap items-center gap-2" role="group" aria-label="Lane filters">
          <FilterSelect
            label="Lanes"
            value={laneFilterValue}
            onChange={(value) => explorer.setLaneFilter(value === 'all' ? null : asLaneId(value))}
            options={[
              { value: 'all', label: `All ${LANES.length}` },
              ...LANES.map((lane) => ({ value: lane.id, label: lane.label })),
            ]}
          />
          {SOURCE_LANE_IDS.map((laneId) => {
            const read = explorer.lanes.find((lane) => lane.lane === laneId);
            if (read === undefined) return null;
            if (explorer.laneFilter !== null && explorer.laneFilter !== laneId) return null;
            const counts = facetCounts(laneHits(read));
            if (counts.length === 0) return null;
            const spec = LANE_BY_ID[laneId];
            return (
              <FilterSelect
                key={laneId}
                label={spec.facetLabel}
                name={`${spec.label} ${spec.facetLabel}`}
                value={explorer.facet?.lane === laneId ? explorer.facet.value : ''}
                onChange={(value) =>
                  explorer.setFacet(value === '' ? null : { lane: laneId, value })
                }
                options={[
                  { value: '', label: `Any · ${counts.length} loaded` },
                  ...counts.map((facet) => ({
                    value: facet.id,
                    label: `${facet.label} · ${facet.count}`,
                  })),
                ]}
              />
            );
          })}
          <button
            type="button"
            onClick={() => {
              explorer.setLaneFilter(null);
              explorer.setFacet(null);
            }}
            disabled={!filtersActive}
            className="td-hit border border-edge-subtle px-3 text-2xs uppercase tracking-[0.1em] text-text-secondary hover:border-accent hover:text-text-primary disabled:opacity-40"
          >
            Clear
          </button>
          <span className="ml-auto text-sm text-text-muted" role="status">
            {explorer.answeredLaneCount} of {explorer.lanes.length} lanes answered
            {explorer.anyPending ? ' · reading' : ''}
            {explorer.unansweredLanes.length > 0
              ? ` · ${explorer.unansweredLanes.length} not served`
              : ''}
          </span>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 max-lg:flex-col">
        <LaneGrid>
          {explorer.visibleLanes.map((read) => (
            <Lane
              key={read.lane}
              read={read}
              rows={explorer.laneRows.get(read.lane) ?? []}
              terms={explorer.terms}
              searching={explorer.searching}
              query={explorer.submitted}
              selectedKey={selected?.key}
              onSelect={(hit) => {
                // The row activating the inspector is the element focus
                // returns to when the inspector closes.
                invokerRef.current =
                  document.activeElement instanceof HTMLElement ? document.activeElement : null;
                select(hit);
              }}
              onPeek={onPeek}
            />
          ))}
        </LaneGrid>
        {inspected ? (
          <aside
            aria-label="Inspector"
            className={cn(
              'shrink-0 overflow-auto border-l border-edge-subtle bg-surface-1',
              'w-[22rem] max-xl:w-72',
              'max-lg:max-h-80 max-lg:w-full max-lg:border-l-0 max-lg:border-t',
            )}
          >
            <HitInspector
              key={`${mode}:${inspected.key}`}
              hit={inspected}
              mode={mode}
              terms={explorer.terms}
              onClose={closeInspector}
            />
          </aside>
        ) : null}
      </div>
    </div>
  );
}

function asLaneId(value: string): LaneId | null {
  return LANES.some((lane) => lane.id === value) ? (value as LaneId) : null;
}

/** A native select behind an engraved legend, so every filter is one control
 * with one accessible name and the platform's own keyboard behaviour.
 *
 * `name` widens the accessible name past the visible legend when the legend
 * alone would be ambiguous (three lanes each have a facet); the visible text
 * stays inside the name, so label-in-name holds. The select is capped rather
 * than sized to its widest option, which at 320px pushed the whole register
 * past the aperture's edge. */
function FilterSelect({
  label,
  name,
  value,
  onChange,
  options,
}: {
  label: string;
  name?: string;
  value: string;
  onChange: (value: string) => void;
  options: readonly { value: string; label: ReactNode }[];
}) {
  return (
    <label className="flex min-h-[var(--touch-target-min)] min-w-0 max-w-full items-center gap-2 border border-edge-subtle bg-surface-0 pl-2 pr-1 focus-within:border-accent">
      <span className="td-legend">{label}</span>
      <select
        value={value}
        aria-label={name}
        onChange={(event) => onChange(event.target.value)}
        className="min-h-[calc(var(--touch-target-min)-2px)] min-w-0 max-w-[10rem] bg-transparent pr-1 text-sm text-text-primary outline-none"
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    </label>
  );
}

const QUERY_PARAM = 'q';
const LANE_PARAM = 'lane';

/**
 * The query and lane filter in the address bar, so a result can be left and
 * returned to. `q` restores by submitting: a run is created for it against
 * the current scope, exactly as if it had been typed, and the lanes report
 * whatever that run answers. Nothing about the previous run's results is
 * carried in the URL, because none of it is addressable state.
 */
function useExplorerUrlState(explorer: ExplorerController) {
  const [searchParams, setSearchParams] = useSearchParams();
  const restored = useRef(false);

  useEffect(() => {
    if (restored.current) return;
    restored.current = true;
    const lane = searchParams.get(LANE_PARAM);
    if (lane !== null) explorer.setLaneFilter(asLaneId(lane));
    const query = searchParams.get(QUERY_PARAM);
    if (query !== null && query.trim() !== '') explorer.submit(query);
    // Runs once, on mount, against the URL the page opened with.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const { submitted, laneFilter } = explorer;
  const written = useRef(false);
  useEffect(() => {
    // The first pass is the restore above; writing here would erase the
    // params it is about to apply.
    if (!written.current) {
      written.current = true;
      return;
    }
    const params = new URLSearchParams(searchParams);
    if ((params.get(QUERY_PARAM) ?? '') === submitted && params.get(LANE_PARAM) === laneFilter) {
      return;
    }
    if (submitted === '') params.delete(QUERY_PARAM);
    else params.set(QUERY_PARAM, submitted);
    if (laneFilter === null) params.delete(LANE_PARAM);
    else params.set(LANE_PARAM, laneFilter);
    setSearchParams(params, { replace: true });
    // Only the two owned params drive this; the rest of the search string is
    // carried through untouched.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [submitted, laneFilter]);
}
