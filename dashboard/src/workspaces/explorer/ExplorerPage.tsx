import { ExplorerSplit } from '../../ui/archetypes/ExplorerSplit.tsx';
import { FacetGroup } from '../../ui/search/Facets.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { useExplorerController } from './controller.ts';
import { HitInspector } from './Inspector.tsx';
import { LANE_BY_ID } from './laneChrome.ts';
import { laneHits } from './laneModel.ts';
import { facetCounts } from './model.ts';
import { LaneLegend, LaneReadout, PlannerRunPanel, UnansweredLanes } from './panels.tsx';
import { ResultList } from './ResultList.tsx';
import { Reveal } from './Reveal.tsx';

/**
 * Explorer — one query, three memories.
 *
 * TraceDecay remembers a repository three separate ways, and the honest
 * consequence is that a search is a *fan-out*, not a single ranked list: the
 * code graph, the transcript store, and the fact store each answer for
 * themselves. This surface makes that structure the design. Each memory is a
 * lane with its own identity rail and its own live state; results stay
 * comparable through one row grammar; and because the daemon returns hits but
 * no relevance score, "why this is here" is told with things that are actually
 * true — the daemon's own ordering, the fields whose text really contains the
 * term, and measured quantities (graph degree, fact trust) that always name
 * the field they came from.
 *
 * Before a query, the surface is not blank: it browses what each memory holds
 * right now, from the same endpoints' overview shapes.
 *
 * The page itself only composes. `controller.ts` owns the coordinator run and
 * the query state, `laneModel.ts` turns the wire into one typed condition per
 * lane, and the three view modules below render those conditions.
 */
export function ExplorerPage() {
  const explorer = useExplorerController();
  return (
    <ExplorerSplit
      stackOnNarrow
      header={
        <div className="flex shrink-0 flex-col gap-3 border-b border-edge-subtle bg-surface-1 px-4 py-3">
          <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
            <h1 className="text-sm font-semibold tracking-tight">Explorer</h1>
            <p className="text-2xs text-text-muted">
              one coordinator run, three source-local answers
            </p>
          </div>
          <div className="flex min-w-0 flex-col gap-3 lg:flex-row lg:items-start">
            <SearchField
              value={explorer.query}
              onChange={explorer.setQuery}
              onSubmit={explorer.submit}
              onClear={explorer.reset}
              submitted={explorer.submitted}
              label="Search code, sessions, and knowledge"
              placeholder="Search everything the daemon remembers…"
              hint={
                explorer.searching ? (
                  <>
                    showing hits for{' '}
                    <span className="font-medium text-text-secondary">
                      “{explorer.submitted}”
                    </span>{' '}
                    in the order returned by each source · terms are marked where they occur in
                    the payload
                  </>
                ) : (
                  <>
                    quote a phrase to keep it whole · press{' '}
                    <kbd className="rounded-[var(--radius-chip)] border border-edge-subtle px-1">
                      /
                    </kbd>{' '}
                    to focus, <kbd className="rounded-[var(--radius-chip)] border border-edge-subtle px-1">Esc</kbd>{' '}
                    to return to browsing
                  </>
                )
              }
            />
            <div
              className="flex shrink-0 flex-wrap gap-2"
              aria-label="Memory lanes"
              role="group"
            >
              {explorer.lanes.map((read, laneIndex) => (
                <Reveal
                  key={read.lane}
                  index={laneIndex}
                  // Three readouts abreast inside a 320px column leaves ~35px
                  // for the label, which clipped every lane name to "CODE …".
                  // Two per row below `lg` gives the name its full measure; the
                  // third stretches across the next row rather than sitting in
                  // a ragged third of one.
                  className="min-w-0 flex-1 basis-[calc(50%-0.25rem)] lg:basis-auto lg:flex-none"
                >
                  <LaneReadout
                    read={read}
                    searching={explorer.searching}
                    active={explorer.laneFilter === read.lane}
                    onToggle={() => explorer.toggleLaneFilter(read.lane)}
                  />
                </Reveal>
              ))}
            </div>
          </div>
        </div>
      }
      filters={
        <div className="flex flex-col gap-4">
          {explorer.searching ? (
            <Reveal index={3}>
              <PlannerRunPanel
                result={explorer.runResult}
                run={explorer.run}
                cancelling={explorer.cancelling}
                onCancel={explorer.cancel}
              />
            </Reveal>
          ) : null}
          {explorer.visibleLanes.map((read) => {
            const counts = facetCounts(laneHits(read));
            if (counts.length === 0) return null;
            return (
              <FacetGroup
                key={read.lane}
                title={LANE_BY_ID[read.lane].facetLabel}
                note="loaded rows"
                facets={counts}
                active={explorer.facet?.lane === read.lane ? explorer.facet.value : null}
                onToggle={(value) =>
                  explorer.setFacet(value === null ? null : { lane: read.lane, value })
                }
              />
            );
          })}
          <Reveal index={4}>
            <LaneLegend lanes={explorer.lanes} searching={explorer.searching} />
          </Reveal>
          {explorer.unansweredLanes.length > 0 ? (
            <Reveal index={5}>
              <UnansweredLanes lanes={explorer.unansweredLanes} />
            </Reveal>
          ) : null}
        </div>
      }
      list={
        <ResultList
          hits={explorer.hits}
          terms={explorer.terms}
          searching={explorer.searching}
          pending={explorer.anyPending}
          query={explorer.submitted}
          laneFilter={explorer.laneFilter}
          facet={explorer.facet}
          unanswered={explorer.unansweredLanes.length > 0}
          answeredLaneCount={explorer.answeredLaneCount}
          laneCount={explorer.lanes.length}
          absence={explorer.absence}
          selectedKey={explorer.selected?.key}
          onSelect={explorer.select}
          onClearFacet={() => explorer.setFacet(null)}
          onClearQuery={explorer.reset}
        />
      }
      inspector={
        explorer.selected ? (
          <HitInspector
            hit={explorer.selected}
            terms={explorer.terms}
            onClose={() => explorer.select(null)}
          />
        ) : undefined
      }
    />
  );
}
