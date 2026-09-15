import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { GitBranch, FolderGit2 } from 'lucide-react';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
import { useActivationField } from '../../viz/graph/useActivationField.ts';
import { buildAdjacency, neighborsOf } from '../../viz/graph/adjacency.ts';
import { useEventStreamState, useLiveActivity } from '../../data/sse/useEvents.tsx';
import { CenteredState, ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { Legend, Readout } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { freshnessTier, relativeAge } from '../../ui/time.ts';
import { useScrollTabStop } from '../../ui/useScrollTabStop.ts';
import { useProjectRegistry } from '../../data/query/projectRegistry.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { ProjectsPayloadV1 } from '../../contracts/generated.ts';
import { useScope } from '../../data/scope/store.ts';
import { EnvelopeTruth } from '../../ui/EnvelopeTruth.tsx';
import { SignalPanel } from './SignalPanel.tsx';
import {
  composeRegistryField,
  indexedMass,
  summarizeHoldings,
  type HoldingsSummary,
  type RegistryField,
} from './field.ts';
import { ScopedBrain } from './ScopedBrain.tsx';
import { ProjectInspector } from './ProjectInspector.tsx';
import {
  type ProjectRegistryEntry,
  type ProjectRepoGroup,
} from '../../contracts/generated.ts';

/** Brain. Two surfaces, because the question genuinely changes when a project
 * is selected.
 *
 * Unscoped, the question is "what does this daemon look after?" and the answer
 * is the registry, composed as a measured field (see `field.ts`) rather than a
 * force layout — several dozen unrelated repositories have no shape to
 * discover, so position is spent on measurement instead.
 *
 * Scoped, the question becomes "what does TraceDecay know about THIS project?",
 * which is a different surface entirely (see `ScopedBrain.tsx`). */
export function BrainPage() {
  const [inspectedId, setInspectedId] = useState<string | null>(null);
  const [registryFilter, setRegistryFilter] = useState('');
  const [repositoryView, setRepositoryView] = useState<string | null>(null);
  const scope = useScope((s) => s.scope);
  const projects = useProjectRegistry();
  const registryRead = envelopeReadState(projects.isPending, toEnvelopeResult(projects.data), {
    loading: 'reading the project registry',
    transport: 'the project registry could not be read',
  });

  if (scope.kind === 'project') {
    return <ScopedBrain key={scope.projectId} projectId={scope.projectId} label={scope.label} />;
  }

  return (
    <ReadSection title="Brain" state={registryRead} chrome="centered">
      {(envelope) => {
        const data = envelope.payload;
        switch (data.status) {
          // Each carries the daemon's own `error`, which is the only part that
          // says which registry path was expected or what failed to open it.
          case 'missing_registry':
            return (
              <CenteredState
                title="Project registry is not configured"
                kind="unavailable"
                detail={data.error ?? undefined}
              />
            );
          case 'registry_unavailable':
            return (
              <CenteredState
                title="Project registry read failed"
                kind="unavailable"
                detail={data.error ?? undefined}
              />
            );
          case 'ok':
            break;
          default:
            // `status` is a plain string on the wire, so a value added in
            // `projects.rs` arrives here as an unfamiliar word rather than a
            // parse failure. Name it instead of guessing which of the three
            // known states it resembles.
            return (
              <CenteredState
                title={`Project registry reported an unrecognised status: ${data.status}`}
                kind="unknown"
                detail={data.error ?? undefined}
              />
            );
        }
        // `ok` carries both, but the field is nullable because the two failure
        // responses send it as an explicit null. A null here is a response that
        // contradicts its own status, which is not the same thing as an empty
        // registry and must not render as one.
        const { project_tree: projectTree, summary } = data;
        if (!projectTree || !summary) {
          return (
            <CenteredState title="Project registry response is inconsistent" kind="partial" />
          );
        }
        if (projectTree.length === 0) {
          const measuredEmpty = summary.project_count === 0 && summary.repo_count === 0;
          return (
            <CenteredState
              title={
                measuredEmpty
                  ? 'Project registry contains no projects'
                  : 'Project registry response is inconsistent'
              }
              kind={measuredEmpty ? 'complete_zero_findings' : 'partial'}
            />
          );
        }
        const groups = [...projectTree].sort(
          (a, b) => latestSeen(b) - latestSeen(a),
        );
        const holdings = summarizeHoldings(groups.flatMap((group) => group.projects));
        const inspectedGroup = groups.find((group) => group.projects.some((project) => project.project_id === inspectedId));
        const inspectedProject = inspectedGroup?.projects.find((project) => project.project_id === inspectedId);
        const query = registryFilter.trim().toLowerCase();
        const viewedRepository = repositoryView == null ? undefined : groups.find((group) => group.git_common_dir === repositoryView);
        const matchingGroups = groups.map((group) => ({ ...group, projects: group.projects.filter((project) =>
          [project.label, project.project_id, project.canonical_root].some((value) => value.toLowerCase().includes(query)),
        ) })).filter((group) => group.projects.length > 0);
        return (
          <div className="flex h-full min-h-0 flex-col" onKeyDown={(event) => {
            if (event.key === 'Escape') setInspectedId(null);
          }}>
            <EnvelopeTruth
              envelope={envelope}
              refreshing={projects.isFetching}
              onRefresh={() => void projects.refetch()}
            />
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Brain</h1>
              <span className="text-2xs text-text-muted">
                {summary.repo_count} repositories · {summary.project_count} projects
                {summary.truncated ? ' · truncated' : ''}
              </span>
            </div>
            {/* The brain is the surface, not a banner above a list: the canvas
             * takes every pixel the viewport can spare, readouts sit on it as
             * instrument HUD, and the registry becomes a dense side rail that
             * remains the canvas's accessible equivalent. */}
            <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
              {/* Below `lg` this is a vertical stack, and the column has to be
                * allowed its natural height. Pinned to a share of the viewport
                * it both squeezed the field and overflowed, and what would not
                * fit painted straight through the registry rail beneath it.
                * The shell's `main` is the scroll container, so giving the
                * stack its real height simply makes the page scroll. From `lg`
                * the two panes split the viewport and each owns its overflow
                * again. */}
              <div className="relative flex shrink-0 flex-col p-3 lg:min-h-0 lg:flex-1">
                {viewedRepository ? <nav aria-label="Brain camera breadcrumb" className="mb-2 flex flex-wrap items-center gap-2 text-xs">
                  <button type="button" className="td-hit underline" onClick={() => setRepositoryView(null)}>Registry overview</button>
                  <span aria-hidden> / </span><span>{viewedRepository.label} repository · {viewedRepository.projects.length} registered {viewedRepository.projects.length === 1 ? 'project' : 'projects'}</span>
                </nav> : null}
                <RegistryFieldView groups={groups} repository={viewedRepository} inspectedId={inspectedId} onInspect={setInspectedId} />
              </div>
              <RegistryRail>
                {/* Keep row coordinates stable between pointer-down and click:
                  * inspection must not insert content above its own trigger. */}
                <div className="h-96 shrink-0 overflow-auto">
                  {inspectedProject && inspectedGroup ? <ProjectInspector project={inspectedProject} group={inspectedGroup} onClose={() => setInspectedId(null)} onRepository={() => setRepositoryView(inspectedGroup.git_common_dir)} /> : <div className="flex h-full flex-col justify-center gap-3 border border-edge-subtle p-4 text-xs text-text-muted">
                    <h2 className="font-semibold text-text-primary">Project inspection</h2>
                    <p>Hover or focus a project to inspect its exact registry evidence.</p>
                    <p>Click or Enter selects project scope. Escape dismisses inspection.</p>
                  </div>}
                </div>
                <label className="text-2xs">Search project registry
                  <input type="search" value={registryFilter} onChange={(event) => setRegistryFilter(event.target.value)} className="mt-1 w-full border border-edge-subtle bg-surface-0 p-2 text-xs" />
                </label>
                {query ? <p className="text-2xs text-text-muted">{matchingGroups.reduce((count, group) => count + group.projects.length, 0)} matching projects · canvas retains the full registry</p> : null}
                {/* The counts that are the same on every row, said once. Every
                  * project in a real registry holds exactly one store and three
                  * to five artifacts, so "1 ST · 4 ART" printed forty-four times
                  * was one fact with forty-three echoes. */}
                {holdings?.uniformLine ? (
                  <p className="text-3xs leading-relaxed text-text-muted">
                    {holdings.uniformLine}
                  </p>
                ) : null}
                {matchingGroups.map((group, index) => (
                  <RepoGroupCard
                    key={`${group.git_common_dir ?? group.label}#${index}`}
                    group={group}
                    holdings={holdings}
                    onInspect={setInspectedId}
                  />
                ))}
              </RegistryRail>
            </div>
          </div>
        );
      }}
    </ReadSection>
  );
}

/** The registry side rail is a scroll container from `lg` and an ordinary
 * block below it, so its tab stop is measured from the rendered box (see
 * `useScrollTabStop`) instead of hard-coded — a literal `tabIndex={0}` gave
 * every keyboard user on a narrow screen a dead stop in front of the cards. */
function RegistryRail({ children }: { children: ReactNode }) {
  const railRef = useRef<HTMLElement>(null);
  const railTabStop = useScrollTabStop(railRef);
  return (
    <aside
      ref={railRef}
      aria-label="Project registry"
      tabIndex={railTabStop}
      className="flex w-full shrink-0 flex-col gap-2 border-t border-edge-subtle p-3 lg:w-80 lg:min-h-0 lg:overflow-auto lg:border-l lg:border-t-0"
    >
      {children}
    </aside>
  );
}

function toEnvelopeResult(
  result: ReturnType<typeof useProjectRegistry>['data'],
): EnvelopeResult<ProjectsPayloadV1> | undefined {
  if (!result) return undefined;
  if (result.outcome === 'envelope') {
    return { outcome: 'envelope', envelope: result.envelope };
  }
  if (result.outcome === 'transport') {
    return { outcome: 'transport', state: result.state, detail: result.detail };
  }
  return undefined;
}

/** The all-projects field. Position, size and brightness are all measurements
 * of the registry — recency across, indexed mass up — so the composition can be
 * read rather than merely looked at. The activation field still fires on real
 * SSE beats, and a repository hub still fires its checkouts, in the one case
 * where such a hub exists at all. */
function RegistryFieldView({
  groups,
  repository,
  inspectedId,
  onInspect,
}: {
  groups: ProjectRepoGroup[];
  repository?: ProjectRepoGroup;
  inspectedId: string | null;
  onInspect: (id: string | null) => void;
}) {
  const selectProject = useScope((s) => s.selectProject);
  const activation = useActivationField(4200);
  const { state: sseState, lastEventAt } = useEventStreamState();
  const { pulses, revision } = useLiveActivity();
  // null until the first pass adopts the connection's current revision: the
  // pulses already in the ring at mount are history, not activity to replay.
  const drawnRevision = useRef<number | null>(null);

  // `groups` is rebuilt (sorted into a fresh array) on every render of this
  // page, and the page re-renders on every live pulse. Memoising on its
  // identity would therefore hand GraphCanvas a new node/edge array per event
  // and force a full renderer teardown plus layout each time. Key the memo on
  // what the topology and the measurements actually ARE instead, so the canvas
  // is rebuilt only when the registry really changed.
  const groupsRef = useRef(groups);
  groupsRef.current = groups;
  const fieldSignature = groups
    .map(
      (group) =>
        `${group.git_common_dir ?? group.label}|${group.projects
          .map(
            (project) =>
              `${project.project_id}:${project.kind}:${indexedMass(project)}:${bucketStamp(project.last_seen_at)}`,
          )
          .join(',')}`,
    )
    .join(';');

  const field: RegistryField = useMemo(
    () => composeRegistryField(groupsRef.current),
    [fieldSignature],
  );
  const repositoryKey = repository?.git_common_dir;
  const repositoryRef = useRef(repository);
  repositoryRef.current = repository;
  const nodes = useMemo(() => {
    if (!repositoryKey) return field.nodes;
    const ids = new Set(repositoryRef.current?.projects.map((project) => project.project_id));
    ids.add(`repo:${repositoryKey}`);
    return field.nodes.filter((node) => ids.has(node.id));
  }, [field, repositoryKey]);
  const edges = useMemo(() => {
    if (!repositoryKey) return field.edges;
    const ids = new Set(nodes.map((node) => node.id));
    return field.edges.filter((edge) => ids.has(edge.source) && ids.has(edge.target));
  }, [field, nodes, repositoryKey]);
  const extent = repositoryKey ? undefined : field.extent;

  // Propagation reads the drawn edge list, so activation can only ever travel
  // where the viewer can see a relation to travel along. On this field most
  // projects have no drawn relation at all, which is correct: a beat in one
  // repository has nothing to conduct into.
  const adjacency = useMemo(() => buildAdjacency(edges), [edges]);
  const drawnIds = useMemo(() => new Set(nodes.map((node) => node.id)), [nodes]);

  // The brain fires on real identities: each accepted event lights the neuron
  // named by its own exact scope, at an intensity that reflects what actually
  // happened. Only unseen pulses fire (the ring is a decay window, not a log).
  // An unscoped event does not establish which project was touched.
  useEffect(() => {
    if (sseState !== 'live' || revision === drawnRevision.current) return;
    if (drawnRevision.current === null) {
      drawnRevision.current = revision;
      return;
    }
    const unseen = Math.min(revision - drawnRevision.current, pulses.length);
    drawnRevision.current = revision;
    for (const pulse of pulses.slice(pulses.length - unseen)) {
      const projectId = pulse.projectId;
      // A scope naming something this field does not draw fires nothing: heat
      // on an id with no body is heat nobody can see, and it would keep the
      // render loop awake resolving an invisible decay.
      if (!projectId || !drawnIds.has(projectId)) continue;
      const energy = strikeIntensity(pulse.family);
      activation.strike([projectId], energy);
      // One synaptic hop along the field's own edges — which exist only where
      // several checkouts share a git directory. It lands at a third the
      // energy, so the conducting edge lights from the end where the event
      // actually happened.
      const hop = neighborsOf(adjacency, projectId);
      if (hop.length > 0) activation.strike(hop, energy / 3);
    }
  }, [pulses, revision, sseState, adjacency, drawnIds, activation]);

  // Stable across renders so the canvas effect never re-runs for a new handler
  // identity; the current registry is read through the ref at click time.
  const handleSelect = useCallback(
    (id: string | null) => {
      if (id == null || id.startsWith('repo:')) return;
      const project = groupsRef.current
        .flatMap((group) => group.projects)
        .find((candidate) => candidate.project_id === id);
      if (project) selectProject(project.project_id, project.label);
    },
    [selectProject],
  );

  const totals = groups
    .flatMap((g) => g.projects)
    .reduce(
      (acc, p) => ({
        stores: acc.stores + p.store_count,
        artifacts: acc.artifacts + p.artifact_count,
      }),
      { stores: 0, artifacts: 0 },
    );

  return (
    <>
      {/* Readouts reserve space so no measured body is hidden behind chrome. */}
      <div className="pointer-events-none mb-2 flex shrink-0 flex-wrap items-start gap-2">
        <InstrumentReadout
          items={[
            { label: 'repos', value: groups.length },
            { label: 'projects', value: field.nodes.length - field.sharedRepoCount },
            { label: 'stores', value: totals.stores },
            { label: 'artifacts', value: totals.artifacts },
          ]}
        />
        <SignalPanel pulses={pulses} sseState={sseState} lastEventAt={lastEventAt} onInspectProject={(id) => onInspect(id !== null && drawnIds.has(id) ? id : null)} />
        {repository ? <figure className="border border-edge-subtle p-2">
          <svg width="160" height="90" viewBox="0 0 160 90" role="img" aria-label="Registry minimap: highlighted projects belong to the viewed repository">
            {field.nodes.map((node) => {
              const x = 8 + ((node.x ?? 0) - field.extent.x[0]) / (field.extent.x[1] - field.extent.x[0]) * 144;
              const y = 82 - ((node.y ?? 0) - field.extent.y[0]) / (field.extent.y[1] - field.extent.y[0]) * 74;
              return <circle key={node.id} cx={x} cy={y} r={nodes.includes(node) ? 3 : 1.5} fill="currentColor" className={nodes.includes(node) ? 'text-accent' : 'text-text-muted'} />;
            })}
          </svg>
          <figcaption className="text-2xs">Registry overview · repository highlighted</figcaption>
        </figure> : null}
      </div>
      <GraphCanvas
        cameraControls
        inspectedId={inspectedId}
        // Keep the exact target available while the pointer moves into its
        // interactive DOM inspector. Escape or Dismiss clears it explicitly.
        onInspect={(id) => { if (id !== null) onInspect(id); }}
        nodes={nodes}
        edges={edges}
        fill
        // The field has a fixed aspect (five columns across a mass axis) and
        // the camera fits it whole, so a canvas far taller than it is wide
        // shrinks the whole composition into a band with dead space above and
        // below. On a phone the canvas is therefore sized in viewport WIDTHS,
        // which keeps its shape near the field's own; from `md` up there is
        // enough width that a generous height is the right trade again.
        canvasClassName="min-h-[64vw] max-h-[84vw] md:max-h-none md:min-h-[55vh] lg:min-h-0"
        extent={extent}
        activation={activation}
        selectedId={null}
        onSelect={handleSelect}
        ariaLabel={repository ? `${repository.label} repository: ${repository.projects.length} registered projects. Exact repository relationships; original measured positions retained. Project registry is the accessible equivalent.` : fieldDescription(field)}
        fallbackDescription="the Project registry beside this field remains available as a text alternative"
        encoding={{
          body: 'project / repo hub',
          size: 'indexed mass / categorical hub',
          hue: 'project kind',
          signal: 'recency / activation',
          relation: 'shared checkout',
        }}
        caption={repository ? <p>{repository.git_common_dir} · exact registry relation · project mass and recency retain the overview measurements</p> : <FieldAxis field={field} />}
      />
    </>
  );
}

/** The horizontal axis, printed. The field's columns are a real measurement and
 * a reader cannot infer their order or their bounds from the picture alone, so
 * they are stated: name, the age each column actually bounds, how many projects
 * fell into it, and a rail giving that count a length. */
function FieldAxis({ field }: { field: RegistryField }) {
  const busiest = field.columns.reduce((max, column) => Math.max(max, column.count), 0);
  return (
    <div className="flex flex-col gap-1.5">
      {/* Short enough to survive a 320px rail: the full sentence is the
        * paragraph below, and a legend that truncates to "INDEXED M…" states
        * nothing. */}
      <Legend>recency across · mass up</Legend>
      <div className="flex flex-wrap border-y border-edge-subtle bg-surface-1">
        {field.columns.map((column) => (
          <div
            key={column.id}
            className="min-w-0 flex-1 basis-24 border-l border-edge-subtle px-2.5 py-1.5 first:border-l-0"
          >
            <Readout
              label={column.label}
              value={column.count}
              unit={column.bound}
              fraction={busiest > 0 ? column.count / busiest : null}
              size="sm"
            />
          </div>
        ))}
      </div>
      <p className="text-2xs leading-relaxed text-text-muted">
        Recency glow spans now to {formatHorizon(field.vitalityHorizonDays)}, the
        age nine in ten projects here are younger than.{' '}
        {field.mass.total > 0 && field.mass.lowerHalfCount > field.mass.total / 2
          ? `Mass is lopsided: ${field.mass.lowerHalfCount} of ${field.mass.total} projects hold ${field.mass.floor}–${field.mass.median} indexed units; the heaviest at ${field.mass.ceiling} sets the top.`
          : ''}{' '}
        {field.sharedRepoCount > 0
          ? `${field.sharedRepoCount} shared ${field.sharedRepoCount === 1 ? 'repository is' : 'repositories are'} wired to their checkouts; all other projects stand alone.`
          : 'No repository has multiple checkouts, so the wire returned no relation to draw.'}
      </p>
    </div>
  );
}

/** The vitality horizon in the shortest form that keeps it readable. */
function formatHorizon(days: number): string {
  if (days < 2) return `${Math.round(days * 24)} h`;
  if (days < 60) return `${days < 10 ? days.toFixed(1) : Math.round(days)} d`;
  return `${Math.round(days / 30)} mo`;
}

function fieldDescription(field: RegistryField): string {
  const occupied = field.columns
    .filter((column) => column.count > 0)
    .map((column) => `${column.count} ${column.label}`)
    .join(', ');
  return `Registry field: projects placed by when they were last seen (${occupied || 'none'}) and by indexed mass, which runs from ${field.mass.floor} to ${field.mass.ceiling} across ${field.mass.total} projects. Brightness is recency, full now and out at ${formatHorizon(field.vitalityHorizonDays)}, the age nine in ten of them are younger than. The project registry list alongside is the accessible equivalent.`;
}

/** Which recency column a timestamp lands in, as a memo key. Keying the field
 * memo on the raw timestamp would recompose the layout on any clock tick; the
 * layout only actually changes when a project crosses a column boundary. */
function bucketStamp(lastSeenAt: number): number {
  return Math.floor((Date.now() / 1000 - lastSeenAt) / 3600);
}

/** Corner-bracketed instrument readout floating on the canvas: the counts that
 * used to occupy four tall tiles, rendered as one hairline strip so the brain
 * keeps the space. Pointer-transparent so it never steals a graph drag. */
export function InstrumentReadout({
  items,
}: {
  items: ReadonlyArray<{ label: string; value: number }>;
}) {
  return (
    <div className="flex max-w-full select-none items-stretch">
      <span aria-hidden className="w-2 border-y border-l border-accent/40" />
      {/* The counts and their names were a step apart on the type scale, which
       * on a HUD floating over a dark field made the whole strip read as one
       * grey ribbon. Setting the figures on the display tier and the names on
       * the legend tier puts the two ends of the scale side by side, so the
       * numbers carry from across the room and the labels stay quiet. */}
      {/* Term before description in the DOM; `flex-col-reverse` keeps the figure
        * above its name on screen, so the reading order is fixed without moving
        * a pixel. */}
      <dl className="flex min-w-0 flex-wrap items-end gap-x-5 gap-y-2 bg-surface-0/75 px-3.5 py-2 backdrop-blur-sm">
        {items.map((item) => (
          <div key={item.label} className="flex flex-col-reverse gap-1">
            <dt className="td-legend">{item.label}</dt>
            <dd
              className="td-display text-lg text-text-primary"
              data-cell="numeric"
            >
              {item.value.toLocaleString()}
            </dd>
          </div>
        ))}
      </dl>
      <span aria-hidden className="w-2 border-y border-r border-accent/40" />
    </div>
  );
}

/** Firing intensity by event family: structural change reads brightest, a
 * heartbeat is only a breath. Unknown families fire at a middling default
 * rather than going dark — a new event family is still real activity. */
function strikeIntensity(family: string): number {
  if (family === 'heartbeat') return 0.22;
  if (family === 'project_registry_changed') return 0.95;
  if (family === 'storage_telemetry_invalidated') return 0.6;
  if (family.startsWith('code_index')) return 0.8;
  if (family === 'hook_activity') return 0.85;
  if (family === 'tool_call_activity') return 0.7;
  if (family === 'session_ingest_activity') return 0.65;
  return 0.5;
}

function RepoGroupCard({
  group,
  holdings,
  onInspect,
}: {
  group: ProjectRepoGroup;
  holdings: HoldingsSummary | null;
  onInspect: (id: string | null) => void;
}) {
  return (
    <section className="rounded-[var(--radius-card)] border border-edge-subtle bg-surface-1">
      <header className="flex items-center gap-2 border-b border-edge-subtle px-3 py-2">
        <FolderGit2 aria-hidden size={14} className="text-text-muted" />
        <h2 className="min-w-0 truncate text-xs font-semibold">{group.label}</h2>
        {/* Count and noun from the one array this header heads. `project_count`
          * is set from `projects.len()` in `project_registry.rs`, so preferring
          * it while pluralising from the array could only ever disagree by
          * printing "3 project" over one row — a contract drift rendered as a
          * typo. */}
        <span className="text-2xs text-text-muted">
          {group.projects.length} {group.projects.length === 1 ? 'project' : 'projects'}
        </span>
        <RecencyDot lastSeenAt={latestSeen(group)} className="ml-auto" />
      </header>
      <div>
        {group.projects.map((project) => (
          <ProjectRow
            key={`${project.project_id}:${project.canonical_root}`}
            project={project}
            holdings={holdings}
            onInspect={onInspect}
          />
        ))}
      </div>
    </section>
  );
}

function ProjectRow({
  project,
  holdings,
  onInspect,
}: {
  project: ProjectRegistryEntry;
  holdings: HoldingsSummary | null;
  onInspect: (id: string | null) => void;
}) {
  const scope = useScope((s) => s.scope);
  const selectProject = useScope((s) => s.selectProject);
  const selected =
    scope.kind === 'project' && scope.projectId === project.project_id;
  const branch = project.default_branch ?? project.branches[0];
  return (
    <button
      type="button"
      onClick={() => selectProject(project.project_id, project.label)}
      onFocus={() => onInspect(project.project_id)}
      onPointerEnter={() => onInspect(project.project_id)}
      aria-pressed={selected}
      className={cn(
        'flex w-full flex-col gap-1 border-b border-edge-subtle px-3 py-2 text-left last:border-b-0',
        'hover:bg-surface-2',
        selected && 'bg-accent/10',
      )}
    >
      {/* Five columns on one line asked for roughly 385px inside a 296px rail,
       * so the age column was simply clipped off the right edge of every row in
       * the registry. The same five facts stack into three lines that fit:
       * identity and age, then where it lives, then what it holds. Dense, but
       * composed -- nothing here is allowed to run off its own card. */}
      <span className="flex w-full items-baseline gap-2">
        <RecencyDot lastSeenAt={project.last_seen_at} className="self-center" />
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-text-primary">
          {project.label}
        </span>
        {project.is_active ? (
          <span className="td-legend shrink-0 bg-accent/15 px-1.5 py-1 text-text-primary">
            active
          </span>
        ) : null}
        <span
          className="td-value shrink-0 text-2xs text-text-muted"
          data-cell="numeric"
        >
          {relativeAge(project.last_seen_at, Date.now() / 1000)}
        </span>
      </span>
      <span
        className="td-value block truncate pl-3.5 text-2xs text-text-muted"
        title={project.canonical_root}
      >
        {project.project_root}
      </span>
      <span className="flex w-full items-baseline gap-2 pl-3.5">
        {branch ? (
          <span className="inline-flex min-w-0 shrink items-center gap-1 text-2xs text-text-secondary">
            <GitBranch aria-hidden size={11} className="shrink-0" />
            <span className="truncate">{branch}</span>
          </span>
        ) : null}
        <span aria-hidden className="td-rule" />
        {/* The row carries the channel that actually varies across the
          * registry — graph scopes span 0 to 242 here — plus any other channel
          * that departs from what the rail stated above it. A project holding
          * five artifacts where everything else holds four IS a reading, and
          * must not be swallowed by the summary. */}
        <span
          className="td-legend shrink-0 text-text-muted"
          data-cell="numeric"
        >
          {holdingsLabel(project, holdings)}
        </span>
      </span>
    </button>
  );
}

/** Recency as a quiet luminance signal, not an alarm color: bright accent for
 * activity within a day, dimming with age, hollow when dormant for a month. */
export function RecencyDot({
  lastSeenAt,
  className,
}: {
  lastSeenAt: number;
  className?: string;
}) {
  const tier = freshnessTier(Math.max(0, Date.now() / 1000 - lastSeenAt));
  let style: string;
  switch (tier) {
    case 'live':
      style = 'bg-accent';
      break;
    case 'recent':
      style = 'bg-accent/60';
      break;
    case 'aging':
      style = 'bg-accent/30';
      break;
    case 'dormant':
      style = 'border border-edge-strong bg-transparent';
      break;
    default: {
      const unhandled: never = tier;
      return unhandled;
    }
  }
  return (
    <span
      aria-hidden
      className={cn('size-1.5 shrink-0 rounded-full', style, className)}
    />
  );
}

/** The per-row holdings label: the varying channel, plus any channel that
 * differs from the value the rail has already stated for everything else. */
function holdingsLabel(
  project: ProjectRegistryEntry,
  holdings: HoldingsSummary | null,
): string {
  if (!holdings) {
    return `${project.store_count} st · ${project.artifact_count} art`;
  }
  const parts: string[] = [];
  if (holdings.artifacts.uniform == null && project.artifact_count !== holdings.artifacts.mode) {
    parts.push(`${project.artifact_count} art`);
  }
  if (holdings.stores.uniform == null) {
    parts.push(`${project.store_count} st`);
  }
  // A registry where literally every channel agrees still has to say something
  // on the row rather than render an empty cell.
  return parts.length > 0 ? parts.join(' · ') : `${indexedMass(project)} indexed`;
}

function latestSeen(group: ProjectRepoGroup): number {
  return group.projects.reduce((max, p) => Math.max(max, p.last_seen_at), 0);
}
