import { useState } from 'react';
import type { ReactNode } from 'react';
import type { LucideIcon } from 'lucide-react';
import { Boxes, FolderGit2, GitBranch, GitFork, GitPullRequest, ScrollText } from 'lucide-react';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { StateChip } from '../../ui/StateChip';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  DeliveryProjectsPayloadSchema,
  type ProjectRegistryEntry,
  type ProjectRepoGroup,
} from './contracts.ts';

/** Delivery: the daemon's git-delivery surface as served by `/api/projects` —
 * registered repositories, their branches, and the primary/worktree checkouts
 * mapped to each. Commit history, pull-request, and review state are not served
 * over the dashboard API (no advisory route in src/dashboard/mod.rs), and
 * per-worktree index freshness is typed unsupported; both render as truthful
 * typed-unavailable pipeline stages rather than invented data. */
export function DeliveryPage() {
  const projects = useLegacy(
    ['delivery', 'projects'],
    '/api/projects',
    DeliveryProjectsPayloadSchema,
  );
  const [selected, setSelected] = useState<ProjectRegistryEntry | null>(null);

  const payload = projects.data?.outcome === 'ok' ? projects.data.data : undefined;
  const activeBranch =
    payload?.project_tree
      ?.flatMap((group) => group.projects)
      .find((entry) => entry.is_active)?.default_branch ?? undefined;

  return (
    <div className="flex h-full flex-col overflow-hidden">
      <header className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Delivery</h1>
        <span className="text-2xs text-text-muted">
          repositories, branches, and worktrees indexed for this workspace
        </span>
        {activeBranch ? (
          <span className="ml-auto inline-flex h-5 items-center gap-1 rounded-[var(--radius-chip)] border border-accent/40 bg-accent/10 px-1.5 text-2xs text-text-primary">
            <GitBranch aria-hidden size={11} />
            {activeBranch}
          </span>
        ) : null}
      </header>

      <div className="min-h-0 flex-1">
        <ExplorerSplit
          filters={
            <div className="flex flex-col gap-3">
              <LegacyBoundary
                title="Delivery"
                pending={projects.isPending}
                result={projects.data}
              >
                {(data) => {
                  const tree = data.project_tree ?? [];
                  const entries = tree.flatMap((group) => group.projects);
                  const worktrees = entries.filter((entry) => entry.kind === 'worktree').length;
                  const branches = new Set<string>();
                  for (const group of tree) for (const branch of group.branches) branches.add(branch);
                  return (
                    // "repositories" is 13 letterspaced caps in a 74px cell, so
                    // every tile in this 2x2 was printing a clipped label
                    // ("REPOSIT…", "CHECKO…"). The counts are the same counts;
                    // the names just fit now.
                    <div className="grid grid-cols-2 gap-2">
                      <StatTile label="repos" value={data.summary?.repo_count ?? tree.length} />
                      <StatTile label="branches" value={branches.size} />
                      <StatTile label="worktrees" value={worktrees} />
                      <StatTile
                        label="checkouts"
                        value={data.summary?.project_count ?? entries.length}
                      />
                    </div>
                  );
                }}
              </LegacyBoundary>

              <div className="flex flex-col gap-2">
                <span className="text-2xs font-medium uppercase tracking-wide text-text-muted">
                  Pipeline
                </span>
                <PipelineStage icon={GitPullRequest} label="Pull requests & review">
                  <StateChip kind="unsupported" detail="not served by the daemon API" />
                </PipelineStage>
                <PipelineStage icon={ScrollText} label="Index freshness">
                  <StateChip kind="unsupported" detail="generation read port unwired" />
                </PipelineStage>
              </div>
            </div>
          }
          list={
            <LegacyBoundary
              title="Repositories"
              pending={projects.isPending}
              result={projects.data}
            >
              {(data) => {
                if (data.status === 'missing_registry') {
                  return (
                    <div className="flex h-full items-center justify-center p-6">
                      <StateChip kind="unknown" detail="no project registry available" />
                    </div>
                  );
                }
                const tree = data.project_tree ?? [];
                if (tree.length === 0) {
                  return (
                    <p className="p-6 text-center text-sm text-text-muted">
                      no repositories registered in this workspace
                    </p>
                  );
                }
                return (
                  <div className="flex flex-col">
                    {tree.map((group, index) => (
                      <RepoGroupSection
                        key={group.git_common_dir ?? group.label ?? String(index)}
                        group={group}
                        selectedId={selected?.project_id ?? null}
                        onSelect={setSelected}
                      />
                    ))}
                    {data.truncated ? (
                      <p className="px-3 py-2 text-2xs text-text-muted">
                        result truncated — raise the registry limit for more repositories
                      </p>
                    ) : null}
                  </div>
                );
              }}
            </LegacyBoundary>
          }
          inspector={
            selected ? (
              <InspectorPanel title="Checkout" onClose={() => setSelected(null)}>
                <KeyValueTree value={selected} />
              </InspectorPanel>
            ) : undefined
          }
        />
      </div>
    </div>
  );
}

function RepoGroupSection({
  group,
  selectedId,
  onSelect,
}: {
  group: ProjectRepoGroup;
  selectedId: string | null;
  onSelect: (entry: ProjectRegistryEntry) => void;
}) {
  return (
    <div className="flex flex-col">
      <div className="sticky top-0 z-[1] flex items-center gap-2 border-b border-edge-subtle bg-surface-1 px-3 py-1.5">
        <FolderGit2 aria-hidden size={12} className="shrink-0 text-text-muted" />
        <span className="min-w-0 flex-1 truncate text-2xs font-semibold uppercase tracking-wide text-text-secondary">
          {group.label}
        </span>
        <span className="tabular shrink-0 text-2xs text-text-muted">
          {group.branches.length} {group.branches.length === 1 ? 'branch' : 'branches'}
        </span>
      </div>
      {group.projects.map((entry) => {
        const { icon: Icon, label: kindLabel } = kindPresentation(entry.kind);
        return (
          <DataRow
            key={entry.project_id}
            selected={selectedId === entry.project_id}
            onSelect={() => onSelect(entry)}
          >
            <Icon aria-hidden size={13} className="shrink-0 text-text-muted" />
            <span className="min-w-0 flex-1 truncate font-mono">{entry.label}</span>
            {entry.default_branch ? (
              <span className="inline-flex shrink-0 items-center gap-1 text-2xs text-text-muted">
                <GitBranch aria-hidden size={11} />
                {entry.default_branch}
              </span>
            ) : null}
            <span className="shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs text-text-muted">
              {kindLabel}
            </span>
            {entry.is_active ? (
              <span
                className="size-1.5 shrink-0 rounded-full bg-accent"
                title="active project"
                aria-label="active project"
              />
            ) : null}
          </DataRow>
        );
      })}
    </div>
  );
}

function PipelineStage({
  icon: Icon,
  label,
  children,
}: {
  icon: LucideIcon;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5 rounded-[var(--radius-chip)] bg-surface-2 px-2.5 py-2">
      <span className="flex items-center gap-1.5 text-2xs font-medium text-text-secondary">
        <Icon aria-hidden size={12} className="text-text-muted" />
        {label}
      </span>
      {children}
    </div>
  );
}

function kindPresentation(kind: string): { icon: LucideIcon; label: string } {
  switch (kind) {
    case 'primary':
      return { icon: FolderGit2, label: 'primary' };
    case 'worktree':
      return { icon: GitFork, label: 'worktree' };
    default:
      return { icon: Boxes, label: kind || 'project' };
  }
}
