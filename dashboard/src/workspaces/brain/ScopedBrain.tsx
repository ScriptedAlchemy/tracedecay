import { useMemo, useRef } from 'react';
import { GitBranch, FolderGit2 } from 'lucide-react';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
import { ActivationField } from '../../viz/graph/activation.ts';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { Legend, Meter, Readout } from '../../ui/instrument.tsx';
import { elideStart, splitBytes, splitCount } from '../../ui/format.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { useScope } from '../../data/scope/store.ts';
import { relativeTime } from './BrainPage.tsx';
import {
  ProjectContextPayloadSchema,
  ScopedAnalyticsOverviewSchema,
  ScopedGraphOverviewSchema,
  ScopedMemoryStatusSchema,
  ScopedSubgraphPayloadSchema,
  type ProjectContextPayload,
  type ProjectStore,
} from './contracts.ts';

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
 *   The project-scoped gateway, `/api/projects/{id}/…`, answers only while that
 *   project's graph is MOUNTED. That is where the code-graph field, the memory
 *   bank and the session analytics come from. When it is not mounted the daemon
 *   says so plainly (404, "registered project graph is not mounted") and this
 *   surface says so too, next to everything the registry does know. Nothing is
 *   estimated, and nothing from the all-projects view is reused as a stand-in.
 */
export function ScopedBrain({ projectId, label }: { projectId: string; label: string }) {
  const selectAllProjects = useScope((s) => s.selectAllProjects);

  // The registry backbone. Read by absolute id rather than through the scoped
  // gateway — `/api/projects` is deliberately never rewritten by scope (see
  // `scopedUrl`), and this read must resolve for a project whose graph is not
  // mounted, which is exactly when the rest of this surface cannot.
  const context = useLegacy(
    ['project-context', projectId],
    `/api/projects/${encodeURIComponent(projectId)}`,
    ProjectContextPayloadSchema,
  );

  // Scoped reads. `useLegacy` rewrites each of these through the project
  // gateway for the current scope, so the paths below are written unscoped.
  const subgraph = useLegacy(
    ['brain', 'subgraph'],
    '/api/plugins/graph/subgraph',
    ScopedSubgraphPayloadSchema,
  );
  const overview = useLegacy(
    ['brain', 'graph-overview'],
    '/api/plugins/graph/overview',
    ScopedGraphOverviewSchema,
  );
  const memory = useLegacy(
    ['brain', 'memory-status'],
    '/api/plugins/holographic/status',
    ScopedMemoryStatusSchema,
  );
  const analytics = useLegacy(
    ['brain', 'analytics'],
    '/api/plugins/analytics/overview',
    ScopedAnalyticsOverviewSchema,
  );

  const activationRef = useRef(new ActivationField({ halfLifeMs: 3200 }));
  const graph = subgraph.data?.outcome === 'ok' ? subgraph.data.data : null;
  const nodes = useMemo(
    () =>
      (graph?.nodes ?? []).map((node) => ({
        id: node.id,
        label: node.name ?? node.qualified_name ?? node.id,
        kind: node.kind,
        degree: node.degree ?? 1,
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

  const totals = overview.data?.outcome === 'ok' ? overview.data.data.totals : null;
  const bank =
    memory.data?.outcome === 'ok' && memory.data.data.exists
      ? (memory.data.data.memory ?? null)
      : null;
  const usage =
    analytics.data?.outcome === 'ok' ? (analytics.data.data.usage ?? null) : null;

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
          className="ml-auto shrink-0 border border-edge-subtle px-2 py-1 text-2xs text-text-secondary hover:bg-surface-2 hover:text-text-primary"
        >
          all projects
        </button>
      </div>
      <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
        <div className="relative flex min-h-0 flex-1 flex-col p-3">
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
          </div>
          {subgraph.isPending ? (
            <p className="flex min-h-[55vh] items-center justify-center p-6 text-center text-sm text-text-muted md:min-h-0 md:flex-1">
              composing this project's graph neighbourhood…
            </p>
          ) : nodes.length > 0 ? (
            <GraphCanvas
              nodes={nodes}
              edges={edges}
              fill
              canvasClassName="min-h-[55vh] md:min-h-0"
              activation={activationRef.current}
              ariaLabel={`${label} code graph: ${nodes.length} symbols, ${edges.length} relations. The stores and branches listed alongside are the accessible equivalent.`}
              caption={
                <>
                  {nodes.length} of this project's most connected symbols ·{' '}
                  {edges.length} real relations between them
                  {graph?.capped?.nodes ? ' · capped by the daemon' : ''} · size =
                  connectedness · hover isolates a neighbourhood, click fires it
                </>
              }
            />
          ) : (
            <UnmountedGraphField label={label} context={context.data} />
          )}
        </div>
        <aside
          aria-label={`What TraceDecay holds for ${label}`}
          tabIndex={0}
          className="flex w-full shrink-0 flex-col gap-3 overflow-auto border-t border-edge-subtle p-3 lg:w-80 lg:border-l lg:border-t-0"
        >
          <LegacyBoundary
            title="Project"
            pending={context.isPending}
            result={context.data}
          >
            {(data) => <ProjectHoldings data={data} />}
          </LegacyBoundary>
          {usage && (usage.by_category?.length ?? 0) > 0 ? (
            <ActivityByCategory
              categories={usage.by_category ?? []}
              total={usage.event_count ?? null}
            />
          ) : null}
        </aside>
      </div>
    </div>
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
      <dl className="flex min-w-0 flex-wrap items-end gap-x-5 gap-y-2 bg-surface-0/75 px-3.5 py-2 backdrop-blur-sm">
        {items.map((item) => (
          <div key={item.label} className="flex flex-col gap-1">
            <dd className="td-display text-lg text-text-primary" data-cell="numeric">
              {item.value}
              {item.unit ? <span className="td-unit ml-0.5">{item.unit}</span> : null}
            </dd>
            <dt className="td-legend">{item.label}</dt>
          </div>
        ))}
      </dl>
      <span aria-hidden className="w-2 border-y border-r border-accent/40" />
    </div>
  );
}

/**
 * What the field shows when this project's code graph is not mounted.
 *
 * The daemon mounts one project's graph at a time, so for every other
 * registered project the scoped gateway truthfully answers 404. That is a fact
 * about the daemon, not a failure of this surface, so the field states the
 * situation and then spends its space on what the registry genuinely does hold:
 * the stores on disk, the branches indexed inside them, and how much they
 * weigh. An apology would have been shorter and would have shown less.
 */
function UnmountedGraphField({
  label,
  context,
}: {
  label: string;
  context: Parameters<typeof LegacyBoundary>[0]['result'];
}) {
  const data =
    context && (context as { outcome: string }).outcome === 'ok'
      ? ((context as { outcome: 'ok'; data: ProjectContextPayload }).data)
      : null;
  const stores = data?.stores ?? [];
  const branches = stores.flatMap((store) => store.graph_scopes ?? []);
  const bytes = stores
    .flatMap((store) => store.artifacts ?? [])
    .reduce((sum, artifact) => sum + (artifact.size_bytes ?? 0), 0);
  const weight = splitBytes(bytes || null);
  return (
    <div className="td-graticule flex min-h-[55vh] flex-col justify-end gap-3 rounded-[var(--radius-card)] border border-edge-subtle/60 bg-surface-0 p-5 md:min-h-0 md:flex-1">
      <div className="flex flex-col gap-2">
        <Legend>graph field · not mounted</Legend>
        <p className="max-w-prose text-xs leading-relaxed text-text-secondary">
          The daemon keeps one project's code graph mounted at a time, and right
          now that is not {label}. There is no neighbourhood to draw here until
          it is — so nothing is drawn. Everything below is read from the
          registry, which answers for {label} either way.
        </p>
      </div>
      <div className="flex flex-wrap border-y border-edge-subtle bg-surface-1">
        {[
          { label: 'stores', ...splitCount(stores.length) },
          { label: 'branches indexed', ...splitCount(branches.length) },
          { label: 'on disk', value: weight.value, unit: weight.unit },
          { label: 'checkouts', ...splitCount(data?.aliases?.length ?? null) },
        ].map((item) => (
          <div
            key={item.label}
            className="min-w-0 flex-1 basis-32 border-l border-edge-subtle px-3 py-2.5 first:border-l-0"
          >
            <Readout label={item.label} value={item.value} unit={item.unit} size="lg" />
          </div>
        ))}
      </div>
    </div>
  );
}

/** The registry backbone, rendered: stores, the branches indexed inside each,
 * the artifacts they weigh, and the paths this project has been checked out
 * at. Available for every registered project, mounted or not. */
function ProjectHoldings({ data }: { data: ProjectContextPayload }) {
  const project = data.project;
  const aliases = [...(data.aliases ?? [])].sort(
    (a, b) => b.last_seen_at - a.last_seen_at,
  );
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
      {(data.stores ?? []).map((store) => (
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

function StoreCard({ store }: { store: ProjectStore }) {
  const scopes = store.graph_scopes ?? [];
  const artifacts = store.artifacts ?? [];
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
      <div className="flex border-b border-edge-subtle">
        <div className="min-w-0 flex-1 px-3 py-2">
          <Readout label="branches" value={scopes.length} size="sm" />
        </div>
        <div className="min-w-0 flex-1 border-l border-edge-subtle px-3 py-2">
          <Readout label="artifacts" value={artifacts.length} size="sm" />
        </div>
        <div className="min-w-0 flex-1 border-l border-edge-subtle px-3 py-2">
          <Readout label="on disk" value={weight.value} unit={weight.unit} size="sm" />
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
                  <span className="flex w-20 shrink-0 flex-col items-end gap-1">
                    <span
                      className="td-value text-2xs leading-none text-text-secondary"
                      data-cell="numeric"
                    >
                      {size.value}
                      {size.unit ? <span className="td-unit ml-1">{size.unit}</span> : null}
                    </span>
                    <Meter
                      fraction={(artifact.size_bytes ?? 0) / heaviest}
                      className="h-[3px] w-full"
                      align="right"
                    />
                  </span>
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
            <span className="flex w-20 shrink-0 flex-col items-end gap-1">
              <span
                className="td-value text-2xs leading-none text-text-secondary"
                data-cell="numeric"
              >
                {row.events.toLocaleString()}
              </span>
              <Meter
                fraction={ceiling > 0 ? row.events / ceiling : null}
                className="h-[3px] w-full"
                align="right"
              />
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}
