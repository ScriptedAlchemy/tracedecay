import { useMemo, useRef } from 'react';
import { GitBranch, FolderGit2 } from 'lucide-react';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
import { ActivationField } from '../../viz/graph/activation.ts';
import { CenteredState, LegacyBoundary } from '../../ui/ReadSection.tsx';
import { FigureRail, Readout } from '../../ui/instrument.tsx';
import { elideStart, splitBytes, splitCount } from '../../ui/format.ts';
import { useScrollTabStop } from '../../ui/useScrollTabStop.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { useProjectEntry } from '../../data/query/projectRegistry.ts';
import { useScope } from '../../data/scope/store.ts';
import { relativeTime } from './BrainPage.tsx';
import {
  AnalyticsOverviewPayloadV1Schema,
  GraphOverviewPayloadV1Schema,
  GraphSubgraphPayloadV1Schema,
  MemoryStatusPayloadV1Schema,
  type GraphSubgraphPayloadV1,
  type ProjectContextPayloadV1,
  type ProjectStoreContext,
} from '../../contracts/generated.ts';

/**
 * The Brain, scoped to one project: "what does TraceDecay actually know about
 * this project?"
 *
 * Selecting a project used to change nothing here — the surface still drew the
 * whole registry, so the one gesture that should have produced the most
 * detailed view produced the least. It is composed from exactly two tiers of
 * real daemon reads, and it never blurs them together:
 *
 *   The registry backbone, `GET /api/projects/{id}`, resolves for every
 *   registered project. It carries the stores, the graph scopes (branches)
 *   inside them, the artifacts on disk with their byte sizes, and every path
 *   the project has been checked out at.
 *
 *   The project-scoped gateway, `/api/projects/{id}/…`, supplies the code graph,
 *   memory bank and session analytics when those reads resolve. The legacy
 *   client preserves transport/schema outcomes but not the server's error body,
 *   so this surface never guesses that a generic failure means "not mounted".
 */
export function ScopedBrain({ projectId, label }: { projectId: string; label: string }) {
  const selectAllProjects = useScope((s) => s.selectAllProjects);

  // The holdings rail is a scroll container at `lg` and an ordinary block
  // below it, so whether it needs a tab stop is a question about the rendered
  // box rather than a constant.
  const holdingsRef = useRef<HTMLElement>(null);
  const holdingsTabStop = useScrollTabStop(holdingsRef);

  // The registry backbone. Read by absolute id rather than through the scoped
  // gateway — `/api/projects` is deliberately never rewritten by scope (see
  // `scopedUrl`), and this read must resolve for a project whose graph is not
  // mounted, which is exactly when the rest of this surface cannot.
  // The shared per-project registry read: the same key and route the scope bar
  // reconciles from, so the two cannot disagree about what this project is
  // called, it is fetched once, and a registry change invalidates both.
  const context = useProjectEntry(projectId);

  // Scoped reads. `useLegacy` rewrites each of these through the project
  // gateway for the current scope, so the paths below are written unscoped.
  const subgraph = useLegacy(
    ['brain', 'subgraph'],
    '/api/plugins/graph/subgraph',
    GraphSubgraphPayloadV1Schema,
  );
  const overview = useLegacy(
    ['brain', 'graph-overview'],
    '/api/plugins/graph/overview',
    GraphOverviewPayloadV1Schema,
  );
  const memory = useLegacy(
    ['brain', 'memory-status'],
    '/api/plugins/holographic/status',
    MemoryStatusPayloadV1Schema,
  );
  const analytics = useLegacy(
    ['brain', 'analytics'],
    '/api/plugins/analytics/overview',
    AnalyticsOverviewPayloadV1Schema,
  );

  const activationRef = useRef(new ActivationField({ halfLifeMs: 3200 }));
  const graph = subgraph.data?.outcome === 'ok' ? subgraph.data.data : null;
  const nodes = useMemo(
    () =>
      (graph?.nodes ?? []).map((node) => ({
        id: node.id,
        label: node.name ?? node.qualified_name ?? node.id,
        kind: node.kind,
        degree: node.degree ?? undefined,
      })),
    [graph],
  );
  const edges = useMemo(
    () =>
      (graph?.edges ?? []).map((edge) => ({
        source: edge.source,
        target: edge.target,
        kind: edge.kind,
      })),
    [graph],
  );

  // Graph totals as measured. `graph_api.rs` answers 500 `read_failed` when a
  // count query fails, so a 200 carries counts that were really taken and a
  // zero among them is an empty graph. The rule here used to blank all three
  // whenever any one was zero, on the stated grounds that the response "cannot
  // distinguish zero data from a query failure" — it can, by status code, and
  // the rule cost a project with an indexed graph and no edges its node count
  // as well.
  const totals = overview.data?.outcome === 'ok' ? overview.data.data.totals : null;

  // `exists` is the memory bank reporting whether it is there, and `error`
  // carries why when it is not. Reading `memory` regardless would render its
  // zeros as a measured empty bank for a project that has no bank at all.
  const memoryRead = memory.data?.outcome === 'ok' ? memory.data.data : null;
  const bank = memoryRead?.exists === true ? memoryRead.memory : null;

  // Two `available` flags, both required by the generated contract, and both
  // load-bearing: `event_count` is 0 when the store did not answer, so reading
  // the number without the flags turns an absent analytics store into a
  // project where nothing has happened.
  const analyticsRead = analytics.data?.outcome === 'ok' ? analytics.data.data : null;
  const usage =
    analyticsRead?.available === true && analyticsRead.usage.available ? analyticsRead.usage : null;

  // Named per source, so a dash in the readout is accounted for rather than
  // being left to read as zero. Only for sources that answered and declared
  // themselves unavailable — a read still in flight, or one that failed, is
  // already reported by its own boundary.
  const unmeasured = [
    memoryRead !== null && memoryRead.exists === false
      ? `Memory: ${memoryRead.error || 'this project has no memory bank.'}`
      : null,
    analyticsRead !== null && analyticsRead.available !== true
      ? 'Analytics: this project has no analytics store, so no activity has been counted.'
      : null,
    analyticsRead?.available === true && !analyticsRead.usage.available
      ? 'Analytics: the store is present but reported no usage summary.'
      : null,
  ].filter((line): line is string => line !== null);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Brain</h1>
        <span className="min-w-0 truncate text-2xs text-text-muted">
          scoped to {label}
        </span>
        <button
          type="button"
          onClick={selectAllProjects}
          className="td-hit group ml-auto shrink-0"
        >
          <span className="border border-edge-subtle px-2 py-1 text-2xs text-text-secondary group-hover:bg-surface-2 group-hover:text-text-primary">
            all projects
          </span>
        </button>
      </div>
      <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
        {/* Same stacking rule as the all-projects field: natural height in the
          * narrow column (the shell's `main` is the scroll container), split
          * panes from `lg`. */}
        <div className="relative flex shrink-0 flex-col p-3 lg:min-h-0 lg:flex-1">
          {/* Same HUD geometry as the all-projects field, so the two Brains
            * read as one instrument in two states rather than two designs. */}
          <div className="pointer-events-none static z-10 mb-2 flex flex-col items-start gap-2 md:absolute md:inset-x-6 md:top-6 md:mb-0">
            <ScopedReadout
              items={[
                { label: 'nodes', ...splitCount(totals?.nodes ?? null) },
                { label: 'edges', ...splitCount(totals?.edges ?? null) },
                { label: 'files', ...splitCount(totals?.files ?? null) },
                { label: 'facts', ...splitCount(bank?.fact_count ?? null) },
                { label: 'entities', ...splitCount(bank?.entity_count ?? null) },
                { label: 'events', ...splitCount(usage?.event_count ?? null) },
              ]}
            />
            {unmeasured.length > 0 ? (
              <ul className="max-w-sm bg-surface-0/75 px-2 py-1 text-3xs leading-relaxed text-text-muted backdrop-blur-sm">
                {unmeasured.map((line) => (
                  <li key={line}>{line}</li>
                ))}
              </ul>
            ) : null}
          </div>
          <LegacyBoundary
            title={`${label} graph`}
            pending={subgraph.isPending}
            result={subgraph.data}
          >
            {(slice) =>
              nodes.length > 0 ? (
                <GraphCanvas
                  nodes={nodes}
                  edges={edges}
                  fill
                  canvasClassName="min-h-[70vw] md:min-h-[58vh] lg:min-h-0"
                  activation={activationRef.current}
                  ariaLabel={`${label} code graph: ${nodes.length} returned symbols, ${edges.length} returned relations. The stores and branches listed alongside are the accessible equivalent.`}
                  fallbackDescription="the project holdings beside this field remain available as a text alternative"
                  encoding={{
                    body: 'symbol',
                    size: 'connectedness',
                    hue: 'symbol kind',
                    signal: 'click activation',
                    relation: 'relation; activation thickens',
                  }}
                  caption={
                    <>
                      {nodes.length} returned symbols · {edges.length} returned relations
                      {graph?.capped?.nodes || graph?.capped?.edges
                        ? ` · daemon capped ${[
                            graph.capped.nodes ? 'symbols' : null,
                            graph.capped.edges ? 'relations' : null,
                          ]
                            .filter(Boolean)
                            .join(' and ')}`
                        : ''}{' '}
                      · size = connectedness · hover isolates a neighbourhood, click fires it
                    </>
                  }
                />
              ) : (
                <EmptySlice slice={slice} label={label} />
              )
            }
          </LegacyBoundary>
        </div>
        <aside
          ref={holdingsRef}
          aria-label={`What TraceDecay holds for ${label}`}
          // Only where it is really a scroller. Its overflow is applied at `lg`,
          // so below that this is an ordinary block in the page flow — measured
          // at 320 and 768 CSS px as `overflow-y: visible` — and a literal
          // `tabIndex={0}` put a stop that does nothing in front of the holdings
          // on exactly the screens where tabbing is most of the navigation.
          tabIndex={holdingsTabStop}
          className="flex w-full shrink-0 flex-col gap-3 border-t border-edge-subtle p-3 lg:w-80 lg:min-h-0 lg:overflow-auto lg:border-l lg:border-t-0"
        >
          <LegacyBoundary
            title="Project"
            pending={context.isPending}
            result={context.data}
            statusInBody
          >
            {(data) => <ProjectHoldings data={data} />}
          </LegacyBoundary>
          {usage && usage.by_category.length > 0 ? (
            <ActivityByCategory categories={usage.by_category} total={usage.event_count} />
          ) : null}
        </aside>
      </div>
    </div>
  );
}

/**
 * A slice that came back with nothing in it, read through the payload's own
 * account of what it went looking for.
 *
 * The route (`graph_service.rs::subgraph_payload`) fails with 500
 * `read_failed`, so an empty 200 is always an answered read — but *what* it
 * answers depends on the mode it ran in, and the two are not the same claim.
 * An unseeded slice draws from the whole graph, so empty means the graph holds
 * nothing. A seeded slice that found no seed means the search matched nothing,
 * which says nothing at all about whether the project is indexed. This surface
 * only ever requests the default slice, but reading `mode` rather than
 * assuming it keeps the sentence true if that ever changes — and the previous
 * text ("cannot distinguish empty data from query failure") was false either
 * way, since the status code distinguishes them.
 */
function EmptySlice({ slice, label }: { slice: GraphSubgraphPayloadV1; label: string }) {
  if (slice.mode === 'default') {
    return (
      <CenteredState title={`No symbols are indexed for ${label}`} kind="complete_zero_findings" />
    );
  }
  if (slice.seed_id === null) {
    return <CenteredState title="No symbol matched this slice request" kind="complete_zero_findings" />;
  }
  return (
    <CenteredState
      title={`Nothing is connected to ${slice.seed_id} in this graph`}
      kind="complete_zero_findings"
    />
  );
}

/** The readouts on the scoped HUD. Every cell renders an em dash when its read
 * did not resolve, which is the point: a project whose graph is not mounted has
 * no node count, and showing nothing there is the true report. */
function ScopedReadout({
  items,
}: {
  items: ReadonlyArray<{ label: string; value: string; unit?: string }>;
}) {
  return (
    <div className="flex max-w-full select-none items-stretch">
      <span aria-hidden className="w-2 border-y border-l border-accent/40" />
      {/* Term before description in the DOM; `flex-col-reverse` keeps the figure
        * above its name on screen, so the reading order is fixed without moving
        * a pixel. */}
      <dl className="flex min-w-0 flex-wrap items-end gap-x-5 gap-y-2 bg-surface-0/75 px-3.5 py-2 backdrop-blur-sm">
        {items.map((item) => (
          <div key={item.label} className="flex flex-col-reverse gap-1">
            <dt className="td-legend">{item.label}</dt>
            <dd className="td-display text-lg text-text-primary" data-cell="numeric">
              {item.value}
              {item.unit ? <span className="td-unit ml-0.5">{item.unit}</span> : null}
            </dd>
          </div>
        ))}
      </dl>
      <span aria-hidden className="w-2 border-y border-r border-accent/40" />
    </div>
  );
}

/** The registry backbone, rendered: stores, the branches indexed inside each,
 * the artifacts they weigh, and the paths this project has been checked out
 * at. Available for every registered project, mounted or not. */
function ProjectHoldings({ data }: { data: ProjectContextPayloadV1 }) {
  // The route's own discriminant, honoured before its arrays are read. A
  // non-`ok` body sends `project: null` with empty `stores`/`aliases`, which
  // rendered as a project that simply holds nothing — the same picture a real
  // empty project draws, for a response that measured nothing at all.
  if (data.status !== 'ok') {
    return (
      <CenteredState
        title={`Project registry reported: ${data.status}`}
        kind="unavailable"
        detail={data.error ?? undefined}
      />
    );
  }
  const project = data.project;
  // `aliases` and `stores` are required arrays in the generated contract, so
  // they are read as arrays. A `?? []` here would absorb a contract change
  // into an empty rail rather than surfacing it.
  const aliases = [...data.aliases].sort((a, b) => b.last_seen_at - a.last_seen_at);
  return (
    <>
      {project ? (
        <section className="rounded-[var(--radius-card)] border border-edge-subtle bg-surface-1">
          <header className="flex items-center gap-2 border-b border-edge-subtle px-3 py-2">
            <FolderGit2 aria-hidden size={14} className="text-text-muted" />
            <h2 className="min-w-0 truncate text-xs font-semibold">{project.label}</h2>
            {data.is_active ? (
              <span className="td-legend ml-auto shrink-0 bg-accent/15 px-1.5 py-1 text-text-primary">
                active
              </span>
            ) : null}
          </header>
          <div className="flex flex-col gap-1 px-3 py-2">
            <span
              className="td-value block truncate text-2xs text-text-muted"
              title={project.canonical_root}
            >
              {project.project_root}
            </span>
            <span className="flex items-baseline gap-2">
              {project.default_branch ? (
                <span className="inline-flex min-w-0 items-center gap-1 text-2xs text-text-secondary">
                  <GitBranch aria-hidden size={11} className="shrink-0" />
                  <span className="truncate">{project.default_branch}</span>
                </span>
              ) : null}
              <span aria-hidden className="td-rule" />
              <span className="td-legend shrink-0 text-text-muted" data-cell="numeric">
                seen {relativeTime(project.last_seen_at)}
              </span>
            </span>
          </div>
        </section>
      ) : null}
      {data.stores.map((store) => (
        <StoreCard key={store.store.store_id} store={store} />
      ))}
      {aliases.length > 0 ? (
        <section className="rounded-[var(--radius-card)] border border-edge-subtle bg-surface-1">
          <header className="flex items-center gap-2 border-b border-edge-subtle px-3 py-2">
            <h2 className="text-xs font-semibold">checkouts</h2>
            <span aria-hidden className="td-rule" />
            <span className="td-legend shrink-0 text-text-muted" data-cell="numeric">
              {aliases.length}
            </span>
          </header>
          <ul className="flex flex-col">
            {aliases.slice(0, 12).map((alias) => (
              <li
                key={alias.alias_path}
                className="flex items-baseline gap-2 border-b border-edge-subtle px-3 py-1.5 last:border-b-0"
              >
                <span
                  className="td-value min-w-0 flex-1 truncate text-2xs text-text-secondary"
                  title={alias.alias_path}
                >
                  {elideStart(alias.alias_path, 30)}
                </span>
                <span
                  className="td-legend shrink-0 text-text-muted"
                  data-cell="numeric"
                >
                  {relativeTime(alias.last_seen_at)}
                </span>
              </li>
            ))}
          </ul>
          {aliases.length > 12 ? (
            <p className="td-legend border-t border-edge-subtle px-3 py-1.5 text-text-muted">
              {aliases.length - 12} more not shown
            </p>
          ) : null}
        </section>
      ) : null}
    </>
  );
}

function StoreCard({ store }: { store: ProjectStoreContext }) {
  const scopes = store.graph_scopes;
  const artifacts = store.artifacts;
  const bytes = artifacts.reduce((sum, a) => sum + (a.size_bytes ?? 0), 0);
  const weight = splitBytes(bytes || null);
  const heaviest = artifacts.reduce((max, a) => Math.max(max, a.size_bytes ?? 0), 0);
  return (
    <section className="rounded-[var(--radius-card)] border border-edge-subtle bg-surface-1">
      <header className="flex items-center gap-2 border-b border-edge-subtle px-3 py-2">
        <h2 className="min-w-0 truncate text-xs font-semibold" title={store.store.store_id}>
          {store.store.store_kind ?? 'store'}
        </h2>
        <span aria-hidden className="td-rule" />
        <span className="td-legend shrink-0 text-text-muted">
          {store.store.storage_mode ?? '—'}
        </span>
      </header>
      {/* Three cells across a 296px rail gave each legend about 60px, which
        * clipped "branches" and "artifacts" to "BRANCH…" and "ARTIFAC…" — the
        * two words that say what the numbers are. On disk is the headline (it
        * is the quantity that grows without anyone asking) and the two counts
        * share the row beneath it, where each has half the rail. */}
      <div className="border-b border-edge-subtle px-3 py-2">
        <Readout
          label="on disk"
          value={weight.value}
          unit={weight.unit}
          size="lg"
          note={`${artifacts.length} ${artifacts.length === 1 ? 'artifact' : 'artifacts'}`}
        />
      </div>
      <div className="flex border-b border-edge-subtle">
        <div className="min-w-0 flex-1 px-3 py-2">
          <Readout label="branches" value={scopes.length} size="sm" />
        </div>
        <div className="min-w-0 flex-1 border-l border-edge-subtle px-3 py-2">
          <Readout label="artifacts" value={artifacts.length} size="sm" />
        </div>
      </div>
      {scopes.length > 0 ? (
        <ul className="flex flex-col">
          {scopes.slice(0, 8).map((scope) => (
            <li
              key={scope.graph_scope_id}
              className="flex items-baseline gap-2 border-b border-edge-subtle px-3 py-1.5 last:border-b-0"
            >
              <GitBranch aria-hidden size={11} className="shrink-0 text-text-muted" />
              <span className="td-value min-w-0 flex-1 truncate text-2xs text-text-secondary">
                {scope.branch_name}
              </span>
              {scope.last_synced_at != null ? (
                <span
                  className="td-legend shrink-0 text-text-muted"
                  data-cell="numeric"
                >
                  {relativeTime(scope.last_synced_at)}
                </span>
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
      {artifacts.length > 0 && heaviest > 0 ? (
        <ul className="flex flex-col border-t border-edge-subtle">
          {[...artifacts]
            .sort((a, b) => (b.size_bytes ?? 0) - (a.size_bytes ?? 0))
            .slice(0, 4)
            .map((artifact) => {
              const size = splitBytes(artifact.size_bytes ?? null);
              return (
                <li
                  key={artifact.relpath}
                  className="flex items-center gap-2 px-3 py-1.5"
                >
                  <span
                    className="td-legend min-w-0 flex-1 truncate text-text-muted"
                    title={artifact.relpath}
                  >
                    {artifact.artifact_kind}
                  </span>
                  <FigureRail
                    value={size.value}
                    unit={size.unit}
                    fraction={(artifact.size_bytes ?? 0) / heaviest}
                  />
                </li>
              );
            })}
        </ul>
      ) : null}
    </section>
  );
}

/** What agents have actually been doing in this project, by tool family. Real
 * counts from the project's own analytics store; ranked against the busiest
 * family so the column reads as a distribution. */
function ActivityByCategory({
  categories,
  total,
}: {
  categories: ReadonlyArray<{ category: string; events: number }>;
  total: number | null;
}) {
  const ranked = [...categories].sort((a, b) => b.events - a.events).slice(0, 8);
  const ceiling = ranked.reduce((max, row) => Math.max(max, row.events), 0);
  return (
    <section className="rounded-[var(--radius-card)] border border-edge-subtle bg-surface-1">
      <header className="flex items-center gap-2 border-b border-edge-subtle px-3 py-2">
        <h2 className="text-xs font-semibold">recorded activity</h2>
        <span aria-hidden className="td-rule" />
        {total != null ? (
          <span className="td-legend shrink-0 text-text-muted" data-cell="numeric">
            {total.toLocaleString()} events
          </span>
        ) : null}
      </header>
      <ul className="flex flex-col">
        {ranked.map((row) => (
          <li
            key={row.category}
            className="flex items-center gap-2 border-b border-edge-subtle px-3 py-1.5 last:border-b-0"
          >
            <span className="td-value min-w-0 flex-1 truncate text-2xs text-text-secondary">
              {row.category}
            </span>
            <FigureRail
              value={row.events.toLocaleString()}
              fraction={ceiling > 0 ? row.events / ceiling : null}
            />
          </li>
        ))}
      </ul>
    </section>
  );
}
