import { GitBranch, FolderGit2 } from 'lucide-react';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
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
          <div className="flex h-full flex-col overflow-auto">
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Brain</h1>
              <span className="text-2xs text-text-muted">
                {data.summary.repo_count} repositories · {data.summary.project_count} projects
                {data.summary.truncated ? ' · truncated' : ''}
              </span>
            </div>
            <div className="grid grid-cols-2 gap-3 p-4 md:grid-cols-4">
              <StatTile label="repositories" value={data.summary.repo_count} />
              <StatTile label="projects" value={data.summary.project_count} />
              <StatTile label="stores" value={totals.stores} />
              <StatTile label="graph scopes" value={totals.scopes} />
            </div>
            <div className="flex flex-col gap-3 px-4 pb-4">
              {groups.map((group, index) => (
                <RepoGroupCard
                  key={`${group.git_common_dir ?? group.label}#${index}`}
                  group={group}
                />
              ))}
            </div>
          </div>
        );
      }}
    </LegacyBoundary>
  );
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
        'flex w-full items-center gap-3 border-b border-edge-subtle px-3 py-2 text-left last:border-b-0',
        'hover:bg-surface-2',
        selected && 'bg-accent/10',
      )}
    >
      <RecencyDot lastSeenAt={project.last_seen_at} />
      <span className="min-w-0 flex-1">
        <span className="block truncate text-xs font-medium">
          {project.label}
          {project.is_active ? (
            <span className="ml-2 rounded-[var(--radius-chip)] bg-accent/15 px-1.5 text-2xs text-accent">
              active
            </span>
          ) : null}
        </span>
        <span
          className="block truncate font-mono text-2xs text-text-muted"
          title={project.canonical_root}
        >
          {project.project_root}
        </span>
      </span>
      {branch ? (
        <span className="inline-flex shrink-0 items-center gap-1 text-2xs text-text-muted">
          <GitBranch aria-hidden size={11} />
          <span className="max-w-32 truncate">{branch}</span>
        </span>
      ) : null}
      <span className="tabular w-28 shrink-0 text-right text-2xs text-text-muted">
        {project.store_count} stores · {project.artifact_count} artifacts
      </span>
      <span className="tabular w-20 shrink-0 text-right text-2xs text-text-muted">
        {relativeTime(project.last_seen_at)}
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
