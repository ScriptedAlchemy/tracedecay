import { z } from 'zod';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { KeyValueTree } from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { AnyObject, ProjectsSchema } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';

/** Brain: the all-projects aggregate first (plan 11a scope model). Real data:
 * project registry + capabilities. The connected brain map (Sigma) is the
 * phase-2 canvas; the aggregate overview ships first. */
export function BrainPage() {
  const projects = useLegacy(['projects'], '/api/projects', ProjectsSchema);
  const capabilities = useLegacy(['capabilities'], '/api/capabilities', AnyObject);

  return (
    <LegacyBoundary title="Brain" pending={projects.isPending} result={projects.data}>
      {(data) => {
        const payload = data as { projects?: Array<Record<string, unknown>> };
        const list = payload.projects ?? [];
        return (
          <div className="flex h-full flex-col overflow-auto">
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Brain</h1>
              <span className="text-2xs text-text-muted">
                all projects · {list.length} registered
              </span>
            </div>
            <div className="grid grid-cols-2 gap-3 p-4 md:grid-cols-4">
              <StatTile label="projects" value={list.length} />
              <StatTile
                label="capabilities"
                value={
                  capabilities.data?.outcome === 'ok'
                    ? Object.keys(capabilities.data.data).length
                    : '—'
                }
              />
            </div>
            <OverviewGrid>
              {list.map((project: Record<string, unknown>, i: number) => {
                const name = String(
                  project['name'] ?? project['id'] ?? project['project_id'] ?? `project ${i}`,
                );
                const root = String(project['root'] ?? project['path'] ?? '');
                return (
                  <OverviewCard key={`${name}-${i}`} title={name}>
                    {root ? (
                      <p className="truncate font-mono text-2xs text-text-muted" title={root}>
                        {root}
                      </p>
                    ) : (
                      <KeyValueTree value={project} />
                    )}
                  </OverviewCard>
                );
              })}
            </OverviewGrid>
          </div>
        );
      }}
    </LegacyBoundary>
  );
}

export const _schemas = { z };
