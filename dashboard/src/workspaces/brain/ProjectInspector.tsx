import type { ProjectRegistryEntry, ProjectRepoGroup } from '../../contracts/generated.ts';
import { indexedMass } from './field.ts';

/** Exact registry evidence for both canvas hover and keyboard inspection. */
export function ProjectInspector({ project, group, onClose, onRepository }: {
  project: ProjectRegistryEntry;
  group: ProjectRepoGroup;
  onClose: () => void;
  onRepository: () => void;
}) {
  return (
    <section aria-label="Inspected project" className="border border-accent/50 bg-surface-1 p-3 text-xs">
      <div className="flex items-start justify-between gap-2">
        <h2 className="min-w-0 break-words font-semibold">{project.label}</h2>
        <button type="button" className="td-hit shrink-0" onClick={onClose} aria-label="Dismiss project inspection">×</button>
      </div>
      <dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-2xs [&>dd]:break-all">
        <dt>ID</dt><dd>{project.project_id}</dd>
        <dt>Root</dt><dd>{project.canonical_root}</dd>
        <dt>Kind</dt><dd>{project.kind}</dd>
        <dt>Stores</dt><dd>{project.store_count.toLocaleString()}</dd>
        <dt>Artifacts</dt><dd>{project.artifact_count.toLocaleString()}</dd>
        <dt>Indexed mass</dt><dd>{indexedMass(project).toLocaleString()}</dd>
        <dt>Last seen</dt><dd>{project.last_seen_at} Unix seconds</dd>
        <dt>Recency source</dt><dd>registry last_seen_at</dd>
        <dt>Repository</dt><dd>{group.git_common_dir ?? 'not recorded'}</dd>
      </dl>
      <p className="mt-2 text-2xs text-text-muted">Inspection does not change project scope or record activity.</p>
      {group.git_common_dir ? <button type="button" className="td-hit mt-2 border border-edge-subtle px-2 text-xs" onClick={onRepository}>View repository</button> : null}
    </section>
  );
}
