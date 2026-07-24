import { useEffect, useMemo, useRef } from 'react';
import { GitBranch, FolderGit2 } from 'lucide-react';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
import { ActivationField } from '../../viz/graph/activation.ts';
import { useEventStreamState, useLiveActivity } from '../../data/sse/useEvents.tsx';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { cn } from '../../ui/cn';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { useScope } from '../../data/scope/store.ts';
import {
  ProjectsPayloadSchema,
  type ProjectRegistryEntry,
  type ProjectRepoGroup,
} from './contracts.ts';

/** Brain: the all-projects aggregate first (plan 11a scope model). Repo-grouped
 * registry with recency signal; selecting a project sets the dashboard scope.
 * The connected Sigma brain map is the phase-2 canvas over this same data. */
export function BrainPage() {
  const projects = useLegacy(['projects'], '/api/projects', ProjectsPayloadSchema);

  return (
    <LegacyBoundary title="Brain" pending={projects.isPending} result={projects.data}>
      {(data) => {
        const groups = [...data.project_tree].sort(
          (a, b) => latestSeen(b) - latestSeen(a),
        );
        const totals = groups
          .flatMap((g) => g.projects)
          .reduce(
            (acc, p) => ({
              stores: acc.stores + p.store_count,
              artifacts: acc.artifacts + p.artifact_count,
              scopes: acc.scopes + p.graph_scope_count,
            }),
            { stores: 0, artifacts: 0, scopes: 0 },
          );
        return (
          <div className="flex h-full min-h-0 flex-col">
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Brain</h1>
              <span className="text-2xs text-text-muted">
                {data.summary.repo_count} repositories · {data.summary.project_count} projects
                {data.summary.truncated ? ' · truncated' : ''}
              </span>
            </div>
            {/* The brain is the surface, not a banner above a list: the canvas
             * takes every pixel the viewport can spare, readouts sit on it as
             * instrument HUD, and the registry becomes a dense side rail that
             * remains the canvas's accessible equivalent. */}
            <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
              <div className="relative flex min-h-0 flex-1 flex-col p-3">
                <SynapseMap
                  groups={groups}
                  activeProjectId={data.active_project_id ?? null}
                />
                <InstrumentReadout
                  items={[
                    { label: 'repos', value: data.summary.repo_count },
                    { label: 'projects', value: data.summary.project_count },
                    { label: 'stores', value: totals.stores },
                    { label: 'scopes', value: totals.scopes },
                    { label: 'artifacts', value: totals.artifacts },
                  ]}
                />
              </div>
              <aside
                aria-label="Project registry"
                tabIndex={0}
                className="flex w-full shrink-0 flex-col gap-2 overflow-auto border-t border-edge-subtle p-3 lg:w-80 lg:border-l lg:border-t-0"
              >
                {groups.map((group, index) => (
                  <RepoGroupCard
                    key={`${group.git_common_dir ?? group.label}#${index}`}
                    group={group}
                  />
                ))}
              </aside>
            </div>
          </div>
        );
      }}
    </LegacyBoundary>
  );
}

/** The all-projects synapse map: repositories as ganglia hubs, each checkout
 * a neuron wired to its hub. The active project pulses with live SSE beats;
 * selecting fires real neighborhoods (see GraphCanvas activation). */
function SynapseMap({
  groups,
  activeProjectId,
}: {
  groups: ProjectRepoGroup[];
  activeProjectId: string | null;
}) {
  const selectProject = useScope((s) => s.selectProject);
  const scope = useScope((s) => s.scope);
  const activationRef = useRef(new ActivationField({ halfLifeMs: 4200 }));
  const { state: sseState } = useEventStreamState();
  const { pulses, revision } = useLiveActivity();
  // null until the first pass adopts the connection's current revision: the
  // pulses already in the ring at mount are history, not activity to replay.
  const drawnRevision = useRef<number | null>(null);

  const { nodes, edges } = useMemo(() => {
    const nodes = [] as Array<{ id: string; label: string; kind: string; degree: number }>;
    const edges = [] as Array<{ source: string; target: string; kind?: string }>;
    for (const group of groups) {
      const hubId = `repo:${group.git_common_dir ?? group.label}`;
      nodes.push({
        id: hubId,
        label: group.label,
        kind: 'repository',
        degree: Math.max(group.projects.length, 1) * 2,
      });
      for (const project of group.projects) {
        nodes.push({
          id: project.project_id,
          label: project.label,
          kind: project.kind,
          degree: Math.max(project.store_count + project.graph_scope_count, 1),
        });
        edges.push({ source: hubId, target: project.project_id, kind: 'checkout' });
      }
    }
    return { nodes, edges };
  }, [groups]);

  // The brain fires on real identities: each accepted event lights the neuron
  // named by its own exact scope, at an intensity that reflects what actually
  // happened. Only unseen pulses fire (the ring is a decay window, not a log),
  // and a beat carrying no project scope falls back to the daemon's active
  // project — which is precisely whose state that beat describes.
  useEffect(() => {
    if (sseState !== 'live' || revision === drawnRevision.current) return;
    if (drawnRevision.current === null) {
      drawnRevision.current = revision;
      return;
    }
    const unseen = Math.min(revision - drawnRevision.current, pulses.length);
    drawnRevision.current = revision;
    const field = activationRef.current;
    for (const pulse of pulses.slice(pulses.length - unseen)) {
      const projectId = pulse.projectId ?? activeProjectId;
      if (!projectId) continue;
      field.strike([projectId], strikeIntensity(pulse.family));
      const hub = groups.find((group) =>
        group.projects.some((project) => project.project_id === projectId),
      );
      // The hub carries its checkout's signal at a third the intensity, so a
      // repository glows when any of its worktrees is working.
      if (hub) {
        field.strike(
          [`repo:${hub.git_common_dir ?? hub.label}`],
          strikeIntensity(pulse.family) / 3,
        );
      }
    }
  }, [pulses, revision, sseState, activeProjectId, groups]);

  return (
    <GraphCanvas
      nodes={nodes}
      edges={edges}
      fill
      activation={activationRef.current}
      selectedId={scope.kind === 'project' ? scope.projectId : null}
      onSelect={(id) => {
        if (id == null || id.startsWith('repo:')) return;
        const project = groups
          .flatMap((group) => group.projects)
          .find((candidate) => candidate.project_id === id);
        if (project) selectProject(project.project_id, project.label);
      }}
    />
  );
}

/** Corner-bracketed instrument readout floating on the canvas: the counts that
 * used to occupy four tall tiles, rendered as one hairline strip so the brain
 * keeps the space. Pointer-transparent so it never steals a graph drag. */
function InstrumentReadout({
  items,
}: {
  items: ReadonlyArray<{ label: string; value: number }>;
}) {
  return (
    <div className="pointer-events-none absolute left-6 top-6 flex select-none items-stretch">
      <span aria-hidden className="w-2 border-y border-l border-accent/40" />
      <dl className="flex items-center gap-4 bg-surface-0/70 px-3 py-1.5 backdrop-blur-sm">
        {items.map((item) => (
          <div key={item.label} className="flex items-baseline gap-1.5">
            <dd className="tabular text-sm font-semibold leading-none text-text-primary">
              {item.value.toLocaleString()}
            </dd>
            <dt className="text-2xs uppercase tracking-wider text-text-muted">
              {item.label}
            </dt>
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
  return 0.5;
}

function RepoGroupCard({ group }: { group: ProjectRepoGroup }) {
  return (
    <section className="rounded-[var(--radius-card)] border border-edge-subtle bg-surface-1">
      <header className="flex items-center gap-2 border-b border-edge-subtle px-3 py-2">
        <FolderGit2 aria-hidden size={14} className="text-text-muted" />
        <h2 className="min-w-0 truncate text-xs font-semibold">{group.label}</h2>
        <span className="text-2xs text-text-muted">
          {group.project_count > 0 ? group.project_count : group.projects.length}{' '}
          {group.projects.length === 1 ? 'project' : 'projects'}
        </span>
        <RecencyDot lastSeenAt={latestSeen(group)} className="ml-auto" />
      </header>
      <div>
        {group.projects.map((project) => (
          <ProjectRow
            key={`${project.project_id}:${project.canonical_root}`}
            project={project}
          />
        ))}
      </div>
    </section>
  );
}

function ProjectRow({ project }: { project: ProjectRegistryEntry }) {
  const scope = useScope((s) => s.scope);
  const selectProject = useScope((s) => s.selectProject);
  const selected =
    scope.kind === 'project' && scope.projectId === project.project_id;
  const branch = project.default_branch ?? project.branches[0];
  return (
    <button
      type="button"
      onClick={() => selectProject(project.project_id, project.label)}
      aria-pressed={selected}
      className={cn(
        'flex w-full items-start gap-2 border-b border-edge-subtle px-3 py-2 text-left last:border-b-0',
        'hover:bg-surface-2',
        selected && 'bg-accent/10',
      )}
    >
      <RecencyDot lastSeenAt={project.last_seen_at} className="mt-1.5" />
      {/* Stacked rather than columnar: the rail is ~320px, and the previous
       * fixed-width metadata columns clipped every field at that measure.
       * Each line truncates independently so nothing is ever cut mid-column. */}
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex items-baseline gap-2">
          <span className="min-w-0 flex-1 truncate text-xs font-medium">{project.label}</span>
          {project.is_active ? (
            <span className="shrink-0 rounded-[var(--radius-chip)] bg-accent/15 px-1.5 text-2xs font-medium text-text-primary">
              active
            </span>
          ) : null}
          <span className="tabular shrink-0 text-2xs text-text-muted">
            {relativeTime(project.last_seen_at)}
          </span>
        </span>
        <span
          className="block truncate font-mono text-2xs text-text-muted"
          title={project.canonical_root}
        >
          {project.project_root}
        </span>
        <span className="flex items-center gap-2 text-2xs text-text-muted">
          {branch ? (
            <span className="inline-flex min-w-0 items-center gap-1">
              <GitBranch aria-hidden size={11} className="shrink-0" />
              <span className="truncate">{branch}</span>
            </span>
          ) : null}
          <span className="tabular ml-auto shrink-0">
            {project.store_count} stores · {project.artifact_count} artifacts
          </span>
        </span>
      </span>
    </button>
  );
}

/** Recency as a quiet luminance signal, not an alarm color: bright accent for
 * activity within a day, dimming with age, hollow when dormant for a month. */
function RecencyDot({
  lastSeenAt,
  className,
}: {
  lastSeenAt: number;
  className?: string;
}) {
  const ageDays = (Date.now() / 1000 - lastSeenAt) / 86_400;
  const style =
    ageDays < 1
      ? 'bg-accent'
      : ageDays < 7
        ? 'bg-accent/60'
        : ageDays < 30
          ? 'bg-accent/30'
          : 'border border-edge-strong bg-transparent';
  return (
    <span
      aria-hidden
      className={cn('size-1.5 shrink-0 rounded-full', style, className)}
    />
  );
}

function latestSeen(group: ProjectRepoGroup): number {
  return group.projects.reduce((max, p) => Math.max(max, p.last_seen_at), 0);
}

function relativeTime(epochSeconds: number): string {
  const delta = Date.now() / 1000 - epochSeconds;
  if (delta < 90) return 'now';
  if (delta < 3600) return `${Math.round(delta / 60)}m ago`;
  if (delta < 86_400) return `${Math.round(delta / 3600)}h ago`;
  if (delta < 30 * 86_400) return `${Math.round(delta / 86_400)}d ago`;
  return `${Math.round(delta / (30 * 86_400))}mo ago`;
}
