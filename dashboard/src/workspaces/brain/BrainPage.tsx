import { useMemo, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { GitBranch, FolderGit2 } from 'lucide-react';
import { useEventStreamState, useLiveActivity } from '../../data/sse/useEvents.tsx';
import { CenteredState, ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { Legend } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { freshnessTier, relativeAge } from '../../ui/time.ts';
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
import { BrainField, FieldLegend } from './BrainField.tsx';
import { buildRegistryScene } from './registryScene.ts';
import {
  type ProjectRegistryEntry,
  type ProjectRepoGroup,
} from '../../contracts/generated.ts';

/** Brain. Two surfaces, because the question genuinely changes when a project
 * is selected.
 *
 * Unscoped, the question is "what does this daemon look after?" and the answer
 * is the project list, with recency and mass stated as text (see `field.ts`).
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
        const registryList = (
          <>
            <div className="flex flex-wrap items-end gap-3">
              <label className="text-2xs">Search project registry
                <input type="search" value={registryFilter} onChange={(event) => setRegistryFilter(event.target.value)} className="mt-1 min-h-[var(--touch-target-min)] w-full border border-edge-subtle bg-surface-0 p-2 text-xs sm:w-72" />
              </label>
              {query ? <p className="text-2xs text-text-muted">{matchingGroups.reduce((count, group) => count + group.projects.length, 0)} matching projects</p> : null}
              {/* The counts that are the same on every row, said once. */}
              {holdings?.uniformLine ? (
                <p className="text-3xs leading-relaxed text-text-muted">{holdings.uniformLine}</p>
              ) : null}
            </div>
            {viewedRepository ? <nav aria-label="Brain camera breadcrumb" className="flex flex-wrap items-center gap-2 text-xs">
              <button type="button" className="td-hit underline" onClick={() => setRepositoryView(null)}>Registry overview</button>
              <span aria-hidden> / </span><span>{viewedRepository.label} repository · {viewedRepository.projects.length} registered {viewedRepository.projects.length === 1 ? 'project' : 'projects'}</span>
            </nav> : null}
            <RegistryList groups={viewedRepository ? [viewedRepository] : matchingGroups} holdings={holdings} onInspect={setInspectedId} />
          </>
        );
        const inspection = inspectedProject && inspectedGroup ? <ProjectInspector project={inspectedProject} group={inspectedGroup} onClose={() => setInspectedId(null)} onRepository={() => setRepositoryView(inspectedGroup.git_common_dir)} /> : <p className="text-2xs text-text-muted">Hover or focus a project to inspect its registry evidence. Click or Enter selects project scope. Escape dismisses inspection.</p>;
        return (
          <div className="flex h-full min-h-0 flex-col" onKeyDown={(event) => {
            if (event.key === 'Escape') setInspectedId(null);
          }}>
            <EnvelopeTruth envelope={envelope} refreshing={projects.isFetching} onRefresh={() => void projects.refetch()} />
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Brain</h1>
              <span className="text-2xs text-text-muted">
                {summary.repo_count} repositories · {summary.project_count} projects
                {summary.truncated ? ' · truncated' : ''}
              </span>
              {viewedRepository ? <button type="button" className="td-hit ml-auto text-2xs underline" onClick={() => setRepositoryView(null)}>Registry overview</button> : null}
            </div>
            {/* The field is the page; readouts, the inspector and the exact
              * registry list sit in the rail beside it. */}
            <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
              <section aria-label="Registry field" className="flex min-h-[70vw] flex-1 flex-col p-3 md:min-h-[60vh] lg:min-h-0">
                <RegistryFieldCanvas groups={groups} repository={viewedRepository} inspectedId={inspectedId} onInspect={setInspectedId} />
              </section>
              <aside aria-label="Registry readouts" className="flex w-full shrink-0 flex-col gap-3 border-t border-edge-subtle p-3 lg:w-96 lg:min-h-0 lg:overflow-auto lg:border-l lg:border-t-0">
                <RegistryFieldView groups={groups} repository={viewedRepository} onInspect={setInspectedId} />
                {inspection}
                <section aria-label="Project registry" className="flex flex-col gap-3">{registryList}</section>
              </aside>
            </div>
          </div>
        );
      }}
    </ReadSection>
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

/** Registry measurements beside the project list. Recency, mass, and
 * shared-checkout counts stay as text; the list is the registry. */
function RegistryFieldView({
  groups,
  repository,
  onInspect,
}: {
  groups: ProjectRepoGroup[];
  repository?: ProjectRepoGroup;
  onInspect: (id: string | null) => void;
}) {
  const { state: sseState, lastEventAt } = useEventStreamState();
  const { pulses } = useLiveActivity();
  const field = composeRegistryField(groups);
  const projectIds = new Set(
    groups.flatMap((group) => group.projects.map((project) => project.project_id)),
  );
  const totals = groups.flatMap((group) => group.projects).reduce(
    (acc, project) => ({
      stores: acc.stores + project.store_count,
      artifacts: acc.artifacts + project.artifact_count,
    }),
    { stores: 0, artifacts: 0 },
  );

  return (
    <>
      <div className="flex flex-col items-start gap-2">
        <InstrumentReadout
          items={[
            { label: 'repos', value: groups.length },
            { label: 'projects', value: field.nodes.length - field.sharedRepoCount },
            { label: 'stores', value: totals.stores },
            { label: 'artifacts', value: totals.artifacts },
          ]}
        />
        <SignalPanel
          pulses={pulses}
          sseState={sseState}
          lastEventAt={lastEventAt}
          onInspectProject={(id) => onInspect(id !== null && projectIds.has(id) ? id : null)}
        />
      </div>
      {repository ? (
        <p className="text-2xs leading-relaxed text-text-muted">
          {repository.git_common_dir} · {repository.projects.length} registered{' '}
          {repository.projects.length === 1 ? 'project' : 'projects'}
        </p>
      ) : (
        <FieldAxis field={field} />
      )}
    </>
  );
}

/** The measured registry field. Rebuilt only
 * when the registry's measurements change, never per live pulse. */
function RegistryFieldCanvas({
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
  const groupsRef = useRef(groups);
  groupsRef.current = groups;
  const signature = groups
    .map((group) => `${group.git_common_dir ?? group.label}|${group.projects
      .map((project) => `${project.project_id}:${project.kind}:${indexedMass(project)}:${project.last_seen_at}`)
      .join(',')}`)
    .join(';');
  const scene = useMemo(
    () => buildRegistryScene(composeRegistryField(groupsRef.current), groupsRef.current),
    [signature],
  );
  const repositoryKey = repository?.git_common_dir ?? null;
  const focus = useMemo(() => {
    if (repositoryKey === null) return null;
    const ids = new Set(scene.bodies.filter((body) => body.group === repositoryKey).map((body) => body.id));
    return ids as ReadonlySet<string>;
  }, [scene, repositoryKey]);
  const projectCount = scene.bodies.filter((body) => body.role === 'body').length;
  return (
    <BrainField
      scene={scene}
      inspectedId={inspectedId}
      onInspect={onInspect}
      onSelect={(id) => {
        const project = groupsRef.current.flatMap((group) => group.projects).find((entry) => entry.project_id === id);
        if (project) selectProject(project.project_id, project.label);
      }}
      focus={focus}
      activity
      ariaLabel={repository
        ? `${repository.label} repository in camera focus: ${repository.projects.length} registered projects; other projects recede. The project registry beside it is the exact equivalent.`
        : `Registry field: ${projectCount} projects across recency columns. The project registry beside it is the exact equivalent.`}
      legend={<FieldLegend scene={scene} />}
    />
  );
}

/** Recency columns, mass shape, and whether any checkout relation exists. */
function FieldAxis({ field }: { field: RegistryField }) {
  return (
    <div className="flex flex-col gap-1.5">
      <Legend>recency · mass</Legend>
      <p className="td-value text-2xs text-text-secondary" data-cell="numeric">
        {field.columns.map((column) => `${column.label} ${column.count}`).join(' · ')}
      </p>
      <p className="text-2xs leading-relaxed text-text-muted">
        Recency runs from now to {formatHorizon(field.vitalityHorizonDays)}, the age nine in ten
        projects here are younger than.{' '}
        {field.mass.total > 0 && field.mass.lowerHalfCount > field.mass.total / 2
          ? `Mass is lopsided: ${field.mass.lowerHalfCount} of ${field.mass.total} projects hold ${field.mass.floor}-${field.mass.median} indexed units; the heaviest at ${field.mass.ceiling} sets the top.`
          : ''}{' '}
        {field.sharedRepoCount > 0
          ? `${field.sharedRepoCount} shared ${field.sharedRepoCount === 1 ? 'repository is' : 'repositories are'} wired to their checkouts; all other projects stand alone.`
          : 'No repository has multiple checkouts, so there is no relation to list.'}
      </p>
    </div>
  );
}

function formatHorizon(days: number): string {
  if (days < 2) return `${Math.round(days * 24)} h`;
  if (days < 60) return `${days < 10 ? days.toFixed(1) : Math.round(days)} d`;
  return `${Math.round(days / 30)} mo`;
}

/** Counts that used to occupy four tall tiles, as one strip. */
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

/** Above this many rows the exact list is windowed; below it the DOM is the
 * plain card list, so small registries keep their exact markup. */
const LIST_VIRTUALIZE_AT = 200;
const GROUP_ROW_ESTIMATE = 37;
const PROJECT_ROW_ESTIMATE = 76;

type RegistryRow =
  | { kind: 'group'; group: ProjectRepoGroup }
  | { kind: 'project'; group: ProjectRepoGroup; project: ProjectRegistryEntry };

/** The exact registry list: every project, searchable, with the same inspect
 * and select routes as the field. Windowed with @tanstack/react-virtual once
 * the registry is large, so a registry of thousands mounts a screenful of
 * rows instead of thousands of cards. */
function RegistryList({
  groups,
  holdings,
  onInspect,
}: {
  groups: ProjectRepoGroup[];
  holdings: HoldingsSummary | null;
  onInspect: (id: string | null) => void;
}) {
  const rows: RegistryRow[] = groups.flatMap((group) => [
    { kind: 'group' as const, group },
    ...group.projects.map((project) => ({ kind: 'project' as const, group, project })),
  ]);
  if (rows.length <= LIST_VIRTUALIZE_AT) {
    return (
      <div className="grid gap-2">
        {groups.map((group, index) => (
          <RepoGroupCard key={`${group.git_common_dir ?? group.label}#${index}`} group={group} holdings={holdings} onInspect={onInspect} />
        ))}
      </div>
    );
  }
  return <VirtualRegistryRows rows={rows} holdings={holdings} onInspect={onInspect} />;
}

function VirtualRegistryRows({
  rows,
  holdings,
  onInspect,
}: {
  rows: RegistryRow[];
  holdings: HoldingsSummary | null;
  onInspect: (id: string | null) => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (index) => (rows[index]?.kind === 'group' ? GROUP_ROW_ESTIMATE : PROJECT_ROW_ESTIMATE),
    overscan: 8,
    getItemKey: (index) => {
      const row = rows[index];
      if (!row) return index;
      return row.kind === 'group' ? `group:${row.group.git_common_dir ?? row.group.label}:${index}` : `project:${row.project.project_id}:${row.project.canonical_root}`;
    },
  });
  return (
    <div
      ref={scrollRef}
      tabIndex={0}
      aria-label={`${rows.filter((row) => row.kind === 'project').length.toLocaleString()} registered projects`}
      className="relative max-h-[70vh] overflow-auto border border-edge-subtle bg-surface-1"
    >
      <div style={{ height: virtualizer.getTotalSize(), position: 'relative', width: '100%' }}>
        {virtualizer.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          return (
            <div
              key={item.key}
              data-index={item.index}
              ref={virtualizer.measureElement}
              style={{ position: 'absolute', top: 0, left: 0, width: '100%', transform: `translateY(${item.start}px)` }}
            >
              {row.kind === 'group' ? (
                <RepoGroupHeader group={row.group} />
              ) : (
                <ProjectRow project={row.project} holdings={holdings} onInspect={onInspect} />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function RepoGroupHeader({ group }: { group: ProjectRepoGroup }) {
  return (
    <header className="flex items-center gap-2 border-b border-edge-subtle px-3 py-2">
      <FolderGit2 aria-hidden size={14} className="text-text-muted" />
      <h2 className="min-w-0 truncate text-xs font-semibold">{group.label}</h2>
      {/* Count and noun from the one array this header heads. `project_count`
        * is set from `projects.len()` in `project_registry.rs`, so preferring
        * it while pluralising from the array could only ever disagree by
        * printing "3 project" over one row, a contract drift rendered as a
        * typo. */}
      <span className="text-2xs text-text-muted">
        {group.projects.length} {group.projects.length === 1 ? 'project' : 'projects'}
      </span>
      <RecencyDot lastSeenAt={latestSeen(group)} className="ml-auto" />
    </header>
  );
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
      <RepoGroupHeader group={group} />
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
          * registry, graph scopes span 0 to 242 here, plus any other channel
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
