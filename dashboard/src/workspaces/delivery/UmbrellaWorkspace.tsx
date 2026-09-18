import { CenteredState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { Panel, ReadoutBar } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import type { DeliveryContext } from './deliveryContext.ts';
import { GradeMark, ProviderStateChip, ReadOnlyProviderBadge } from './deliveryChrome.tsx';
import { DELIVERY_LAYOUTS } from './deliveryLocation.ts';
import { gradeLabel, type EvidenceGrade } from './evidence.ts';
import { activeAttention, projectFor } from './inboxFilter.ts';
import { pullRequestStateKind } from './PullRequestInspector.tsx';
import type { Umbrella, UmbrellaMember } from './umbrella.ts';
import { UmbrellaField } from './UmbrellaField.tsx';

const BASES_SENTENCE =
  'Umbrellas form only from served bases: shared Work objective, explicit handoff, session–Git relation, shared agent. Proximity never groups.';

const TABLE_HEADINGS = [
  'Umbrella', 'Basis', 'Identity', 'Grade', 'Pull request',
  'Project', 'Head state', 'Active attention', 'Destination',
] as const;

/**
 * The umbrella Delivery graph. An umbrella is a correlation projection over
 * the admitted inbox, never a provider object: the selected umbrella is the
 * root, every member PR stays separately drillable, every edge shows its
 * basis and grade, and an inferred grouping says so. When the authority served
 * no correlating edge the workspace prints that typed absence, never an
 * empty graph.
 */
export function UmbrellaWorkspace({ context }: { context: DeliveryContext }) {
  const { inbox, location, navigate, umbrellas: projection } = context;
  const authority = projection.authority;
  if (authority.state === 'unavailable') {
    return <CorrelationUnavailable context={context} reason={authority.reason} />;
  }

  const selected =
    projection.umbrellas.find((umbrella) => umbrella.id === location.umbrella) ??
    projection.umbrellas[0] ??
    null;
  const grouped = new Set(
    projection.umbrellas.flatMap((umbrella) => umbrella.members.map((member) => member.id)),
  ).size;
  const spanned = new Set(projection.umbrellas.flatMap((umbrella) => umbrella.projectIds)).size;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ReadoutBar
        label="Umbrella readings"
        elevation="raised"
        items={[
          { label: 'umbrellas', value: projection.umbrellas.length, note: 'one per served basis identity' },
          { label: 'correlating edges', value: authority.edges, note: 'served by the inbox authority' },
          { label: 'PRs grouped', value: grouped, note: `of ${inbox.pull_requests.length} admitted` },
          { label: 'projects spanned', value: spanned, note: `of ${inbox.projects.length} registered` },
          { label: 'unresolved edges', value: projection.unresolvedEdges, note: 'edge to a non-admitted PR' },
        ]}
      />
      <div className="flex flex-wrap items-center gap-3 border-b border-edge-subtle bg-surface-1 px-3 py-2">
        <p className="min-w-0 flex-1 text-3xs text-text-muted">{BASES_SENTENCE}</p>
        <div role="group" aria-label="Layout" className="flex border border-edge-subtle">
          {DELIVERY_LAYOUTS.map((layout) => (
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
      {selected === null ? (
        <CenteredState
          title="No umbrella formed"
          kind="complete_zero_findings"
          detail={`${authority.edges} correlating edges were served, but none joins two admitted pull requests under one basis identity · ${projection.unresolvedEdges} unresolved`}
        />
      ) : location.layout === 'table' ? (
        <MembershipTable context={context} selected={selected} />
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[19rem_minmax(0,1fr)] xl:grid-cols-[19rem_minmax(0,1fr)_22rem]">
          <UmbrellaOutcomes context={context} selected={selected} />
          <div className="flex min-h-64 min-w-0 flex-col border-r border-edge-subtle p-3">
            <p className="mb-2 truncate font-mono text-3xs tracking-[0.08em] text-text-muted">
              Umbrella · {selected.basisLabel} · {selected.identity}
            </p>
            <UmbrellaField
              inbox={inbox}
              rows={inbox.pull_requests}
              projection={projection}
              focusUmbrellaId={selected.id}
              selectedRowId={location.pullRequest}
              selectedUmbrellaId={selected.id}
              onSelectRow={(row) => navigate({ pullRequest: row.id })}
              onSelectUmbrella={(id) => navigate({ umbrella: id })}
            />
            <ul className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 font-mono text-3xs text-text-muted" aria-label="Field legend">
              <li>root = selected umbrella · rails = repositories · edge stroke = grade</li>
              <li className="flex items-center gap-2">
                {(['exact', 'explicit', 'inferred', 'stale'] as const).map((grade) => (
                  <GradeMark key={grade} grade={grade} />
                ))}
              </li>
            </ul>
          </div>
          <UmbrellaInspector context={context} umbrella={selected} className="lg:col-span-2 xl:col-span-1" />
        </div>
      )}
    </div>
  );
}

function CorrelationUnavailable({ context, reason }: { context: DeliveryContext; reason: string }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <CenteredState title="Umbrella correlation unavailable" kind="unavailable" detail={reason} />
      <p className="mx-auto max-w-xl px-3 pb-3 text-center text-xs leading-relaxed text-text-secondary">
        {BASES_SENTENCE}
      </p>
      <p className="pb-4 text-center">
        <button
          type="button"
          className="inline-flex min-h-9 items-center border border-edge-strong px-3 text-xs text-text-primary hover:bg-surface-2"
          onClick={() => context.navigate({ mode: 'inbox' })}
        >
          Back to inbox
        </button>
      </p>
    </div>
  );
}

function UmbrellaOutcomes({ context, selected }: { context: DeliveryContext; selected: Umbrella }) {
  const { navigate, umbrellas } = context;
  return (
    <section aria-label="Umbrella outcomes" className="flex min-h-0 flex-col border-r border-edge-subtle bg-surface-1">
      <header className="border-b border-edge-subtle px-3 py-2">
        <h2 className="td-title">Umbrella outcomes</h2>
        <p className="mt-1 text-3xs text-text-muted">
          {umbrellas.umbrellas.length} correlation projections · ordered by basis strength
        </p>
      </header>
      <ul className="min-h-0 flex-1 overflow-auto">
        {umbrellas.umbrellas.map((umbrella) => {
          const pressed = umbrella.id === selected.id;
          return (
            <li key={umbrella.id} className="border-b border-edge-subtle">
              <button
                type="button"
                aria-pressed={pressed}
                className={cn(
                  'relative flex min-h-16 w-full items-start gap-2 px-3 py-2 text-left hover:bg-surface-2',
                  pressed && 'bg-surface-2',
                )}
                onClick={() => navigate({ umbrella: umbrella.id })}
              >
                {pressed ? <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" /> : null}
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-xs font-medium text-text-primary">{umbrella.basisLabel}</span>
                  <span className="mt-0.5 block truncate font-mono text-3xs text-text-secondary">{umbrella.identity}</span>
                  <span className="mt-1 block truncate font-mono text-3xs text-text-muted">
                    {umbrella.members.length} PRs · {umbrella.projectIds.length} projects ·{' '}
                    {umbrella.activeAttention} active
                  </span>
                </span>
                <GradeMark grade={umbrella.grade} source={umbrella.source} className="mt-0.5" />
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function UmbrellaInspector({
  context,
  umbrella,
  className,
}: {
  context: DeliveryContext;
  umbrella: Umbrella;
  className?: string;
}) {
  return (
    <section
      aria-label="Umbrella inspector"
      className={cn('flex min-h-0 min-w-0 flex-col overflow-auto bg-surface-1', className)}
    >
      <header className="border-b border-edge-subtle px-3 py-3">
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold leading-snug text-text-primary">{umbrella.basisLabel}</h2>
            <p className="mt-1 break-all font-mono text-3xs text-text-secondary">{umbrella.identity}</p>
          </div>
          <GradeMark grade={umbrella.grade} source={umbrella.source} className="mt-1" />
        </div>
        <p className="mt-2 text-3xs leading-relaxed text-text-muted">{gradeSentence(umbrella.grade)}</p>
        <div className="mt-3 flex flex-wrap gap-2">
          <ReadOnlyProviderBadge />
        </div>
      </header>

      <Panel legend={`Members · ${umbrella.members.length}`} className="m-3" bodyClassName="p-0">
        <ul className="divide-y divide-edge-subtle">
          {umbrella.members.map((member) => (
            <MemberRow key={member.id} context={context} member={member} />
          ))}
        </ul>
      </Panel>

      <Panel legend="Per-project provider state" className="mx-3 mb-3" bodyClassName="p-0">
        <ul className="divide-y divide-edge-subtle">
          {umbrella.projectIds.map((projectId) => {
            const project = projectFor(context.inbox, projectId);
            return (
              <li key={projectId} className="flex items-center justify-between gap-2 px-3 py-2">
                <span className="min-w-0 truncate text-xs text-text-primary">{project?.label ?? projectId}</span>
                {project === null ? (
                  <StateChip kind="unknown" detail="project not in the served registry" />
                ) : (
                  <ProviderStateChip state={project.provider_state} />
                )}
              </li>
            );
          })}
        </ul>
      </Panel>
    </section>
  );
}

function MemberRow({ context, member }: { context: DeliveryContext; member: UmbrellaMember }) {
  const { inbox, location, navigate } = context;
  const row = member.pullRequest;
  const selected = location.pullRequest === member.id;
  return (
    <li className={cn('px-3 py-2', selected && 'bg-surface-2')}>
      <div className="flex items-start justify-between gap-2">
        <span className="min-w-0 flex-1">
          <span className="block truncate text-xs font-medium text-text-primary">
            {row.pull_request.identity?.title ?? row.pull_request.label}
          </span>
          <span className="mt-0.5 block truncate font-mono text-3xs text-text-muted">
            {projectFor(inbox, member.projectId)?.label ?? member.projectId} · #
            {row.pull_request.pull_request_id} · {activeAttention(row)} active
          </span>
        </span>
        <StateChip kind={pullRequestStateKind(row.state)} />
      </div>
      <div className="mt-2 flex flex-wrap items-center justify-between gap-2">
        <GradeMark grade={member.edge.grade} source={member.edge.source} />
        <span className="flex gap-2">
          <button
            type="button"
            aria-pressed={selected}
            className={cn(
              'td-hit border border-edge-subtle px-2 text-3xs text-text-primary hover:bg-surface-2',
              selected && 'border-accent',
            )}
            onClick={() => navigate({ pullRequest: member.id })}
          >
            Select
          </button>
          <button
            type="button"
            className="td-hit border border-accent px-2 text-3xs text-text-primary hover:bg-surface-2"
            onClick={() => navigate({ mode: 'journey', pullRequest: member.id })}
          >
            Open journey
          </button>
        </span>
      </div>
    </li>
  );
}

function MembershipTable({ context, selected }: { context: DeliveryContext; selected: Umbrella }) {
  return (
    <div className="min-h-0 flex-1 overflow-auto">
      <table className="w-full border-collapse text-xs" aria-label="Umbrella membership table">
        <thead className="sticky top-0 bg-surface-1 text-left">
          <tr className="border-b border-edge-subtle">
            {TABLE_HEADINGS.map((heading) => (
              <th key={heading} scope="col" className="td-legend px-3 py-2 font-normal">
                {heading}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {context.umbrellas.umbrellas.flatMap((umbrella) =>
            umbrella.members.map((member) => (
              <MembershipRow
                key={`${umbrella.id}:${member.id}`}
                context={context}
                umbrella={umbrella}
                member={member}
                umbrellaSelected={umbrella.id === selected.id}
              />
            )),
          )}
        </tbody>
      </table>
    </div>
  );
}

function MembershipRow({
  context,
  umbrella,
  member,
  umbrellaSelected,
}: {
  context: DeliveryContext;
  umbrella: Umbrella;
  member: UmbrellaMember;
  umbrellaSelected: boolean;
}) {
  const { inbox, location, navigate } = context;
  const row = member.pullRequest;
  const rowSelected = location.pullRequest === member.id;
  const cell = 'px-3 py-2';
  return (
    <tr className={cn('border-b border-edge-subtle align-top', (umbrellaSelected || rowSelected) && 'bg-surface-2')}>
      <td className={cell}>
        <button
          type="button"
          aria-pressed={umbrellaSelected}
          className="break-all text-left font-mono text-3xs text-text-primary hover:underline"
          onClick={() => navigate({ umbrella: umbrella.id })}
        >
          {umbrella.id}
        </button>
      </td>
      <td className={cn(cell, 'text-text-primary')}>{umbrella.basisLabel}</td>
      <td className={cn(cell, 'break-all font-mono text-3xs text-text-secondary')}>{umbrella.identity}</td>
      <td className={cell}>
        <GradeMark grade={member.edge.grade} source={member.edge.source} />
      </td>
      <td className={cell}>
        <button
          type="button"
          aria-pressed={rowSelected}
          className="text-left text-text-primary hover:underline"
          onClick={() => navigate({ pullRequest: member.id })}
        >
          #{row.pull_request.pull_request_id} {row.pull_request.identity?.title ?? row.pull_request.label}
        </button>
      </td>
      <td className={cn(cell, 'font-mono text-3xs text-text-secondary')}>
        {projectFor(inbox, member.projectId)?.label ?? member.projectId}
      </td>
      <td className={cell}>
        <StateChip kind={pullRequestStateKind(row.state)} />
      </td>
      <td className={cn(cell, 'font-mono text-3xs')} data-cell="numeric">
        {activeAttention(row)} / {row.attention.length}
      </td>
      <td className={cell}>
        <button
          type="button"
          className="text-accent hover:underline"
          onClick={() => navigate({ mode: 'journey', pullRequest: member.id })}
        >
          Journey
        </button>
      </td>
    </tr>
  );
}

function gradeSentence(grade: EvidenceGrade): string {
  switch (grade) {
    case 'explicit':
      return 'Grouped by a persisted claim; the outcome is the claim, not repository truth.';
    case 'inferred':
      return 'Grouped by correlation; the basis is named and the grouping is reversible.';
    case 'exact':
    case 'ambiguous':
    case 'stale':
    case 'unavailable':
      return gradeLabel(grade);
    default: {
      const unhandled: never = grade;
      return unhandled;
    }
  }
}
