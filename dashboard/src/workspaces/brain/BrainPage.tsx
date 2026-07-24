import { useEffect, useMemo, useRef } from 'react';
import { GitBranch, FolderGit2 } from 'lucide-react';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
import { ActivationField } from '../../viz/graph/activation.ts';
import { useEventStreamState, useLiveActivity } from '../../data/sse/useEvents.tsx';
import type { LiveActivityPulse, SseConnectionState } from '../../data/sse/connect.ts';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { Meter, Readout } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { useScope } from '../../data/scope/store.ts';
import { summarizeActivity, formatEventAge } from './activitySummary.ts';
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
  const { state: sseState, lastEventAt } = useEventStreamState();
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
    <>
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
      <SignalPanel pulses={pulses} sseState={sseState} lastEventAt={lastEventAt} />
    </>
  );
}

/** Live-signal HUD: connection honesty first, then a compact readout of what
 * the pulse ring actually carries. Every figure here is read off the ring or
 * the connection object at render time — never a timer of its own, so an
 * idle system between real events shows the age of its last real event
 * exactly as long as it stays true, and no longer.
 *
 * Connection state is intentionally NOT derived from activity: `offline` is
 * a genuinely dead stream (the EventSource itself closed) and must never be
 * inferred from "no pulses lately", which is just a quiet system. The
 * daemon's own low-chroma `offline` token is deliberately neutral in this
 * taxonomy — StateChip's icon-plus-label is what actually makes a dead link
 * unmistakable, not a color. */
function SignalPanel({
  pulses,
  sseState,
  lastEventAt,
}: {
  pulses: readonly LiveActivityPulse[];
  sseState: SseConnectionState;
  lastEventAt: number | null;
}) {
  const summary = summarizeActivity(pulses);
  const connectionKind: DomainStateKind =
    sseState === 'live' ? 'ready' : sseState === 'connecting' ? 'loading' : 'offline';
  const ageMs = lastEventAt == null ? null : Date.now() - lastEventAt;
  const topFamily = summary.families[0];
  return (
    <div className="pointer-events-none absolute right-6 top-6 flex max-w-64 select-none items-stretch">
      <span aria-hidden className="w-2 border-y border-l border-accent/40" />
      <div className="flex min-w-0 flex-col gap-2 bg-surface-0/75 px-3.5 py-2 backdrop-blur-sm">
        <div className="flex items-center gap-2">
          <StateChip kind={connectionKind} />
        </div>
        <span className="td-legend">
          {sseState === 'offline'
            ? 'event stream unreachable — not idle, disconnected'
            : ageMs == null
              ? 'no events observed yet'
              : `last event ${formatEventAge(ageMs)}`}
        </span>
        {summary.families.length > 0 ? (
          <dl className="flex flex-col gap-1">
            {summary.families.slice(0, 4).map((entry) => (
              <div key={entry.family} className="flex items-center gap-2">
                <dt className="td-legend w-24 shrink-0 truncate">{entry.label}</dt>
                <Meter
                  fraction={topFamily ? entry.count / topFamily.count : null}
                  className="min-w-8 flex-1"
                />
                <dd
                  className="td-value w-5 shrink-0 text-right text-2xs"
                  data-cell="numeric"
                >
                  {entry.count}
                </dd>
              </div>
            ))}
          </dl>
        ) : null}
        {summary.ratePerMinute != null ? (
          <Readout label="rate" size="sm" value={summary.ratePerMinute.toFixed(1)} unit="/min" />
        ) : null}
      </div>
      <span aria-hidden className="w-2 border-y border-r border-accent/40" />
    </div>
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
      {/* The counts and their names were a step apart on the type scale, which
       * on a HUD floating over a dark field made the whole strip read as one
       * grey ribbon. Setting the figures on the display tier and the names on
       * the legend tier puts the two ends of the scale side by side, so the
       * numbers carry from across the room and the labels stay quiet. */}
      <dl className="flex items-end gap-5 bg-surface-0/75 px-3.5 py-2 backdrop-blur-sm">
        {items.map((item) => (
          <div key={item.label} className="flex flex-col gap-1">
            <dd
              className="td-display text-lg text-text-primary"
              data-cell="numeric"
            >
              {item.value.toLocaleString()}
            </dd>
            <dt className="td-legend">{item.label}</dt>
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
          {relativeTime(project.last_seen_at)}
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
        <span
          className="td-legend shrink-0 text-text-muted"
          data-cell="numeric"
        >
          {project.store_count} st · {project.artifact_count} art
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
