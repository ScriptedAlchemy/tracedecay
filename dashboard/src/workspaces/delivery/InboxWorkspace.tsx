import { GitPullRequest } from 'lucide-react';
import {
  DeliveryAttentionSourceV1Schema,
  DeliveryProviderStateV1Schema,
  type DeliveryInboxPullRequestV1,
} from '../../contracts/generated.ts';
import { CenteredState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { ReadoutBar } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import type { DeliveryContext } from './DeliveryPage.tsx';
import { GradeMark, ProviderStateChip } from './deliveryChrome.tsx';
import type { DeliveryLocation } from './deliveryLocation.ts';
import { membershipGrade, membershipSourceClass, providerServes } from './evidence.ts';
import { activeAttention, edgesFor, projectFor } from './inboxFilter.ts';
import {
  attentionSourceLabel,
  PullRequestInspector,
  pullRequestStateKind,
} from './PullRequestInspector.tsx';
import { relatedAcrossProjects, umbrellasFor } from './umbrella.ts';
import { UmbrellaField } from './UmbrellaField.tsx';

const STATUS_OPTIONS = [
  ['open', 'Open'],
  ['merged', 'Merged'],
  ['closed', 'Closed'],
  ['draft', 'Draft'],
] as const;

/**
 * The global / project-scoped pull request inbox. The list is primary and
 * keyboard-reachable; the field groups correlated PRs without hiding one; the
 * inspector reads the selection; the table is the exact fallback for all of it.
 */
export function InboxWorkspace({
  context,
  rows,
  selectedRow,
}: {
  context: DeliveryContext;
  rows: readonly DeliveryInboxPullRequestV1[];
  selectedRow: DeliveryInboxPullRequestV1 | null;
}) {
  const { inbox, location, navigate, umbrellas } = context;
  const scopedProject = projectFor(inbox, location.project);
  const related =
    location.project === null ? [] : relatedAcrossProjects(umbrellas, location.project);
  const providerNotServing = inbox.projects.filter(
    (project) => !providerServes(project.provider_state),
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ReadoutBar
        label="Inbox readings"
        elevation="raised"
        items={[
          { label: 'projects', value: inbox.projects.length, note: `${providerNotServing.length} without provider reads` },
          { label: 'admitted PRs', value: inbox.pull_requests.length, note: `${rows.length} after filters` },
          {
            label: 'active attention',
            value: inbox.pull_requests.reduce((total, row) => total + activeAttention(row), 0),
          },
          {
            label: 'umbrellas',
            value: umbrellas.authority.state === 'served' ? umbrellas.umbrellas.length : '—',
            note: umbrellas.authority.state === 'served' ? `${umbrellas.authority.edges} correlating edges` : 'correlation unavailable',
          },
          { label: 'omitted projects', value: inbox.omitted_projects },
          { label: 'excluded provider PRs', value: inbox.excluded_pull_requests },
        ]}
      />
      <InboxFilters context={context} />
      <ProjectStrip context={context} />
      {inbox.pull_requests.length === 0 ? (
        <EmptyInbox context={context} />
      ) : location.layout === 'table' ? (
        <InboxTable context={context} rows={rows} related={related} />
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[19rem_minmax(0,1fr)] xl:grid-cols-[19rem_minmax(0,1fr)_22rem]">
          <div className="flex min-h-0 flex-col border-r border-edge-subtle bg-surface-1">
            <PullRequestQueue
              context={context}
              rows={rows}
              selectedId={selectedRow?.id ?? null}
              legend={scopedProject === null ? 'Admitted inbox' : `${scopedProject.label} · project PRs`}
            />
            {scopedProject === null ? null : <RelatedRail context={context} related={related} />}
          </div>
          <div className="flex min-h-64 min-w-0 flex-col border-r border-edge-subtle p-3">
            <UmbrellaField
              inbox={inbox}
              rows={rows}
              projection={umbrellas}
              focusUmbrellaId={null}
              selectedRowId={selectedRow?.id ?? null}
              selectedUmbrellaId={null}
              onSelectRow={(row) => navigate({ pullRequest: row.id })}
              onSelectUmbrella={(umbrella) => navigate({ mode: 'umbrella', umbrella })}
            />
            <FieldLegend />
          </div>
          {selectedRow === null ? (
            <aside className="flex items-center justify-center p-6 text-center text-xs text-text-muted lg:col-span-2 xl:col-span-1">
              Select a pull request to inspect its identity, attention evidence and correlation edges.
            </aside>
          ) : (
            <PullRequestInspector
              context={context}
              row={selectedRow}
              className="lg:col-span-2 xl:col-span-1"
            />
          )}
        </div>
      )}
    </div>
  );
}

function InboxFilters({ context }: { context: DeliveryContext }) {
  const { inbox, location, navigate } = context;
  const select =
    'h-9 border border-edge-subtle bg-surface-0 px-2 text-xs normal-case tracking-normal text-text-primary';
  return (
    <div className="flex flex-wrap items-end gap-3 border-b border-edge-subtle bg-surface-1 px-3 py-2">
      <label className="flex min-w-44 flex-col gap-1 text-3xs uppercase tracking-wider text-text-muted">
        Project
        <select
          aria-label="Project"
          className={select}
          value={location.project ?? ''}
          onChange={(event) => navigate({ project: event.currentTarget.value || null })}
        >
          <option value="">All registered projects</option>
          {inbox.projects.map((project) => (
            <option key={project.project_id} value={project.project_id}>
              {project.label}
            </option>
          ))}
        </select>
      </label>
      <label className="flex min-w-32 flex-col gap-1 text-3xs uppercase tracking-wider text-text-muted">
        Status
        <select
          aria-label="Status"
          className={select}
          value={location.status ?? ''}
          onChange={(event) =>
            navigate({ status: (event.currentTarget.value || null) as DeliveryLocation['status'] })
          }
        >
          <option value="">Any status</option>
          {STATUS_OPTIONS.map(([value, label]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <label className="flex min-w-48 flex-col gap-1 text-3xs uppercase tracking-wider text-text-muted">
        Attention source
        <select
          aria-label="Attention source"
          className={select}
          value={location.attention ?? ''}
          onChange={(event) =>
            navigate({
              attention: (event.currentTarget.value || null) as DeliveryLocation['attention'],
            })
          }
        >
          <option value="">All attention sources</option>
          {DeliveryAttentionSourceV1Schema.options.map((source) => (
            <option key={source} value={source}>
              {attentionSourceLabel(source)}
            </option>
          ))}
        </select>
      </label>
      <label className="flex min-w-40 flex-col gap-1 text-3xs uppercase tracking-wider text-text-muted">
        Provider state
        <select
          aria-label="Provider state"
          className={select}
          value={location.provider ?? ''}
          onChange={(event) =>
            navigate({
              provider: (event.currentTarget.value || null) as DeliveryLocation['provider'],
            })
          }
        >
          <option value="">Any provider state</option>
          {DeliveryProviderStateV1Schema.options.map((state) => (
            <option key={state} value={state}>
              {state.replaceAll('_', ' ')}
            </option>
          ))}
        </select>
      </label>
      <label className="flex min-h-9 items-center gap-2 text-xs text-text-secondary">
        <input
          type="checkbox"
          className="td-check"
          checked={location.unresolvedOnly}
          onChange={(event) => navigate({ unresolvedOnly: event.currentTarget.checked })}
        />
        Unresolved only
      </label>
      <div className="ml-auto flex items-center gap-2">
        {inbox.excluded_pull_requests > 0 ? (
          <p className="text-3xs text-text-muted">
            {inbox.excluded_pull_requests} unrelated provider pull request
            {inbox.excluded_pull_requests === 1 ? '' : 's'} excluded
          </p>
        ) : null}
        <div role="group" aria-label="Layout" className="flex border border-edge-subtle">
          {(['field', 'table'] as const).map((layout) => (
            <button
              key={layout}
              type="button"
              aria-pressed={location.layout === layout}
              className={cn(
                'td-hit px-3 text-3xs uppercase tracking-[0.12em]',
                location.layout === layout ? 'bg-surface-2 text-text-primary' : 'text-text-muted',
              )}
              onClick={() => navigate({ layout })}
            >
              {layout}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

function ProjectStrip({ context }: { context: DeliveryContext }) {
  const { inbox, location, navigate } = context;
  return (
    <div className="flex flex-wrap gap-2 border-b border-edge-subtle px-3 py-2" aria-label="Registered projects">
      {inbox.projects.map((project) => (
        <button
          key={project.project_id}
          type="button"
          aria-pressed={location.project === project.project_id}
          className={cn(
            'flex items-center gap-2 border border-transparent px-2 py-1 text-3xs hover:border-edge-subtle',
            location.project === project.project_id && 'border-edge-strong bg-surface-2',
          )}
          onClick={() =>
            navigate({ project: location.project === project.project_id ? null : project.project_id })
          }
        >
          <span className="font-medium text-text-primary">{project.label}</span>
          <ProviderStateChip state={project.provider_state} />
        </button>
      ))}
    </div>
  );
}

function FieldLegend() {
  return (
    <ul className="mt-2 flex flex-wrap gap-x-4 gap-y-1 font-mono text-3xs text-text-muted" aria-label="Field legend">
      <li>◎ project hub</li>
      <li>● admitted PR · amber dot = active attention</li>
      <li>◌ umbrella root at member centroid</li>
      <li className="flex items-center gap-1">edge grade <GradeMark grade="explicit" /> <GradeMark grade="inferred" /></li>
    </ul>
  );
}

function PullRequestQueue({
  context,
  rows,
  selectedId,
  legend,
}: {
  context: DeliveryContext;
  rows: readonly DeliveryInboxPullRequestV1[];
  selectedId: string | null;
  legend: string;
}) {
  const { inbox, navigate } = context;
  return (
    <section aria-label="Admitted pull requests" className="flex min-h-0 flex-1 flex-col">
      <header className="border-b border-edge-subtle px-3 py-2">
        <h2 className="td-title">{legend}</h2>
        <p className="mt-1 text-3xs text-text-muted">
          Provider rows appear only after an indexed-head join · {rows.length} shown
        </p>
      </header>
      {rows.length === 0 ? (
        <p className="p-3 text-3xs text-text-muted">No admitted pull request matches the current filters.</p>
      ) : (
        <ul className="min-h-0 flex-1 overflow-auto">
          {rows.map((row) => {
            const active = activeAttention(row);
            const project = projectFor(inbox, row.project_id);
            const grouped = umbrellasFor(context.umbrellas, row.id).length;
            return (
              <li key={row.id} className="border-b border-edge-subtle">
                <button
                  type="button"
                  aria-pressed={selectedId === row.id}
                  className={cn(
                    'relative flex min-h-16 w-full items-start gap-2 px-3 py-2 text-left hover:bg-surface-2',
                    selectedId === row.id && 'bg-surface-2',
                  )}
                  onClick={() => navigate({ pullRequest: row.id })}
                >
                  {selectedId === row.id ? (
                    <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" />
                  ) : null}
                  <GitPullRequest className="mt-0.5 h-4 w-4 shrink-0 text-accent" aria-hidden />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-xs font-medium text-text-primary">
                      {row.pull_request.identity?.title ?? row.pull_request.label}
                    </span>
                    <span className="mt-1 block truncate font-mono text-3xs text-text-muted">
                      {project?.label ?? row.project_id} · {row.pull_request.provider} #
                      {row.pull_request.pull_request_id} · {active} active
                      {grouped > 0 ? ` · ${grouped} umbrella${grouped === 1 ? '' : 's'}` : ''}
                    </span>
                  </span>
                  <StateChip kind={pullRequestStateKind(row.state)} />
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

/** Cross-project pull requests correlated to the scoped project, each with
 * the basis and grade that placed it here. */
function RelatedRail({
  context,
  related,
}: {
  context: DeliveryContext;
  related: ReturnType<typeof relatedAcrossProjects>;
}) {
  const { inbox, navigate, umbrellas } = context;
  return (
    <section aria-label="Related pull requests" className="border-t border-edge-subtle">
      <header className="px-3 py-2">
        <h2 className="td-title">Correlated · other projects</h2>
        <p className="mt-1 text-3xs text-text-muted">
          {umbrellas.authority.state === 'served'
            ? `${related.length} qualified by a served basis`
            : 'correlation unavailable · no cross-PR edge served'}
        </p>
      </header>
      {related.length === 0 ? null : (
        <ul>
          {related.map(({ member, umbrella }) => (
            <li key={member.id} className="border-t border-edge-subtle">
              <button
                type="button"
                className="flex w-full flex-col gap-1 px-3 py-2 text-left hover:bg-surface-2"
                onClick={() => navigate({ pullRequest: member.id })}
              >
                <span className="truncate text-xs text-text-primary">
                  {member.pullRequest.pull_request.identity?.title ?? member.pullRequest.pull_request.label}
                </span>
                <span className="flex flex-wrap items-center justify-between gap-2 font-mono text-3xs text-text-muted">
                  <span>
                    {projectFor(inbox, member.projectId)?.label ?? member.projectId} · #
                    {member.pullRequest.pull_request.pull_request_id}
                  </span>
                  <GradeMark grade={umbrella.grade} source={umbrella.source} />
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function InboxTable({
  context,
  rows,
  related,
}: {
  context: DeliveryContext;
  rows: readonly DeliveryInboxPullRequestV1[];
  related: ReturnType<typeof relatedAcrossProjects>;
}) {
  const { inbox, navigate, location } = context;
  const relatedIds = new Set(related.map((entry) => entry.member.id));
  const all = [...rows, ...related.map((entry) => entry.member.pullRequest)];
  return (
    <div className="min-h-0 flex-1 overflow-auto">
      <table className="w-full border-collapse text-xs" aria-label="Admitted pull requests table">
        <thead className="sticky top-0 bg-surface-1 text-left">
          <tr className="border-b border-edge-subtle">
            {['Pull request', 'Project', 'Provider state', 'Head state', 'Active attention', 'Correlation basis · grade', 'Scope', 'Destinations'].map(
              (heading) => (
                <th key={heading} scope="col" className="td-legend px-3 py-2 font-normal">
                  {heading}
                </th>
              ),
            )}
          </tr>
        </thead>
        <tbody>
          {all.map((row) => {
            const project = projectFor(inbox, row.project_id);
            const edges = edgesFor(inbox, row);
            return (
              <tr
                key={row.id}
                className={cn('border-b border-edge-subtle align-top', location.pullRequest === row.id && 'bg-surface-2')}
              >
                <td className="px-3 py-2">
                  <button
                    type="button"
                    className="text-left text-text-primary hover:underline"
                    onClick={() => navigate({ pullRequest: row.id })}
                  >
                    #{row.pull_request.pull_request_id} {row.pull_request.identity?.title ?? row.pull_request.label}
                  </button>
                </td>
                <td className="px-3 py-2 font-mono text-3xs text-text-secondary">{project?.label ?? row.project_id}</td>
                <td className="px-3 py-2">
                  {project === null ? '—' : <ProviderStateChip state={project.provider_state} />}
                </td>
                <td className="px-3 py-2">
                  <StateChip kind={pullRequestStateKind(row.state)} />
                </td>
                <td className="px-3 py-2 font-mono text-3xs" data-cell="numeric">
                  {activeAttention(row)} / {row.attention.length}
                </td>
                <td className="px-3 py-2">
                  <ul className="space-y-1">
                    {edges.map((edge) => (
                      <li key={edge.id}>
                        <GradeMark grade={membershipGrade(edge.basis)} source={membershipSourceClass(edge.basis)} />
                      </li>
                    ))}
                  </ul>
                </td>
                <td className="px-3 py-2 font-mono text-3xs text-text-muted">
                  {relatedIds.has(row.id) ? 'correlated · other project' : 'in scope'}
                </td>
                <td className="px-3 py-2">
                  <span className="flex flex-wrap gap-2">
                    <button
                      type="button"
                      className="text-accent hover:underline"
                      onClick={() => navigate({ mode: 'journey', pullRequest: row.id })}
                    >
                      Journey
                    </button>
                    <button
                      type="button"
                      className="text-accent hover:underline"
                      onClick={() => navigate({ mode: 'review', pullRequest: row.id })}
                    >
                      Review
                    </button>
                  </span>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

/** Zero admitted pull requests, told apart: provider absent versus a real
 * complete-zero join. Neither is a blank list. */
function EmptyInbox({ context }: { context: DeliveryContext }) {
  const { inbox } = context;
  const notServing = inbox.projects.filter((project) => !providerServes(project.provider_state));
  if (notServing.length > 0 && notServing.length === inbox.projects.length) {
    const notConfigured = notServing.every(
      (project) => project.provider_state === 'not_configured' || project.provider_state === 'not_published',
    );
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <CenteredState
          title={notConfigured ? 'Provider not configured' : 'Provider reads unavailable'}
          kind="unavailable"
          detail={
            notConfigured
              ? 'not_published · requires github_read_authority. Registered repositories remain visible, but provider reads are not configured. No unrelated pull request is admitted.'
              : 'No registered project can serve pull requests right now; each state is printed above.'
          }
        />
        <p className="px-3 pb-3 text-center text-xs text-text-secondary">No admitted pull requests</p>
        <p className="px-3 pb-4 text-center text-3xs text-text-muted">
          <a href="/settings" className="text-accent hover:underline">Open Settings · Provider authority</a>
          {' · '}
          scope a project above to continue with local evidence
        </p>
      </div>
    );
  }
  return (
    <CenteredState
      title="No admitted pull requests"
      kind="complete_zero_findings"
      detail="The registry and indexed-head join completed with no admitted pull requests."
    />
  );
}
