import { GitPullRequest } from 'lucide-react';
import type {
  DeliveryAttentionEvidenceV1,
  DeliveryAttentionSourceV1,
  DeliveryAttentionStateV1,
  DeliveryInboxPullRequestV1,
  DeliveryInboxPullRequestStateV1,
} from '../../contracts/generated.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import type { DeliveryContext } from './deliveryContext.ts';
import {
  ControlLink,
  GradeMark,
  IdentityRow,
  ProviderStateChip,
  ReadOnlyProviderBadge,
  microsToIso,
  shortSha,
} from './deliveryChrome.tsx';
import {
  coverageKind,
  membershipBasisDetail,
  membershipBasisLabel,
  membershipGrade,
  membershipHref,
  membershipSourceClass,
  providerOutcomeKind,
  providerStateSentence,
} from './evidence.ts';
import { edgesFor, projectFor } from './inboxFilter.ts';
import { compareHref } from './review.ts';
import { umbrellasFor } from './umbrella.ts';

/**
 * The selected pull request, exactly as the inbox authority served it: its
 * identity, the provider state of its project, each attention source with
 * its evidence, and every membership edge with basis and grade. Destinations
 * are the journey, the review workspace, Code, Loom, and the provider's own
 * URL, read-only throughout.
 */
export function PullRequestInspector({
  context,
  row,
  className,
}: {
  context: DeliveryContext;
  row: DeliveryInboxPullRequestV1;
  className?: string;
}) {
  const { inbox, location, navigate } = context;
  const project = projectFor(inbox, row.project_id);
  const edges = edgesFor(inbox, row);
  const umbrellas = umbrellasFor(context.umbrellas, row.id);
  const identity = row.pull_request.identity;
  const title = identity?.title ?? row.pull_request.label;

  return (
    <section
      aria-label="Pull request detail"
      className={cn('flex min-h-0 min-w-0 flex-col overflow-auto bg-surface-1', className)}
    >
      <header className="border-b border-edge-subtle px-3 py-3">
        <div className="flex items-start gap-2">
          <GitPullRequest className="mt-0.5 h-4 w-4 shrink-0 text-accent" aria-hidden />
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold leading-snug text-text-primary">{title}</h2>
            <p className="mt-1 font-mono text-3xs text-text-muted">
              {row.pull_request.provider} #{row.pull_request.pull_request_id}
              {identity === null
                ? ' · identity not served'
                : ` · ${identity.draft ? 'draft · ' : ''}${identity.state} · +${identity.additions} −${identity.deletions} · ${identity.changed_files} files`}
            </p>
          </div>
          <StateChip kind={pullRequestStateKind(row.state)} />
        </div>
        <div className="mt-3 flex flex-wrap gap-2">
          <button
            type="button"
            className="inline-flex min-h-[var(--touch-target-min)] items-center border border-accent px-3 text-xs text-text-primary hover:bg-surface-2"
            onClick={() => navigate({ mode: 'journey', pullRequest: row.id })}
          >
            Open journey
          </button>
          <button
            type="button"
            className="inline-flex min-h-[var(--touch-target-min)] items-center border border-edge-strong px-3 text-xs text-text-primary hover:bg-surface-2"
            onClick={() => navigate({ mode: 'review', pullRequest: row.id })}
          >
            Open review
          </button>
          <ReadOnlyProviderBadge />
        </div>
      </header>

      <dl className="border-b border-edge-subtle px-3 py-2">
        <IdentityRow label="project" value={project?.label ?? row.project_id} />
        <IdentityRow label="branch" value={row.branch_ref} />
        <IdentityRow label="indexed head" value={shortSha(row.indexed_head_commit_id)} />
        <IdentityRow label="generation" value={row.indexed_generation} />
        <IdentityRow label="repository" value={row.repository_id} />
      </dl>

      {project === null ? null : (
        <Panel legend="Provider" className="m-3" bodyClassName="p-3">
          <div className="flex flex-wrap items-center gap-2">
            <ProviderStateChip state={project.provider_state} />
            <GradeMark grade="exact" source="registry" />
          </div>
          <p className="mt-2 text-3xs leading-relaxed text-text-muted">
            {providerStateSentence(project.provider_state)}
          </p>
          {row.pull_request.operations.length === 0 ? (
            <p className="mt-2 font-mono text-3xs text-text-muted">no provider operation snapshot retained</p>
          ) : (
            <ul className="mt-2 divide-y divide-edge-subtle">
              {row.pull_request.operations.map((operation) => {
                const snapshot = operation.last_complete ?? operation.latest_attempt;
                return (
                  <li key={operation.operation} className="flex items-center justify-between gap-2 py-1">
                    <span className="font-mono text-3xs text-text-secondary">
                      {operation.operation.replaceAll('_', ' ')}
                    </span>
                    {snapshot === null ? (
                      <StateChip kind="unknown" detail="no read" />
                    ) : (
                      <StateChip
                        kind={providerOutcomeKind(snapshot.outcome)}
                        detail={`${operation.last_complete === null ? 'attempt' : 'complete'} · ${microsToIso(snapshot.fetched_at_micros).slice(0, 16)}Z`}
                      />
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </Panel>
      )}

      <div className="grid gap-3 px-3 pb-3">
        {row.attention.map((item) => (
          <Panel key={item.id} legend={attentionSourceLabel(item.source)} bodyClassName="p-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <StateChip kind={attentionStateKind(item.state)} detail={item.coverage} />
              <span className="flex items-center gap-2">
                <StateChip kind={coverageKind(item.coverage)} />
                <span className="font-mono text-3xs text-text-muted">
                  {microsToIso(item.observed_at_micros)}
                </span>
              </span>
            </div>
            {item.evidence.length === 0 ? (
              <p className="mt-2 text-3xs text-text-muted">
                {item.coverage === 'unsupported'
                  ? 'This source has no mounted Delivery authority.'
                  : 'No evidence item is active.'}
              </p>
            ) : (
              <ul className="mt-2 space-y-1">
                {item.evidence.map((evidence, index) => {
                  const id = evidenceIdentity(evidence);
                  const href = evidenceHref(evidence, row);
                  return (
                    <li key={`${id}:${index}`} className="flex items-stretch gap-1">
                      <button
                        type="button"
                        aria-pressed={location.evidence === id}
                        className={cn(
                          'min-w-0 flex-1 break-all border border-edge-subtle px-2 py-1 text-left font-mono text-3xs',
                          location.evidence === id && 'border-accent bg-surface-2',
                        )}
                        onClick={() => navigate({ evidence: id })}
                      >
                        {id}
                      </button>
                      {href === null ? null : (
                        <a
                          href={href}
                          className="inline-flex shrink-0 items-center border border-edge-subtle px-2 font-mono text-3xs text-accent hover:bg-surface-2"
                          title="Open in Code · Compare"
                        >
                          code
                        </a>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
          </Panel>
        ))}
      </div>

      <Panel legend={`Correlation · ${edges.length} edges`} className="mx-3 mb-3" bodyClassName="p-0">
        {edges.length === 0 ? (
          <p className="p-3 text-3xs text-text-muted">No admitted membership edge.</p>
        ) : (
          <ul className="divide-y divide-edge-subtle">
            {edges.map((edge) => {
              const href = membershipHref(edge.basis);
              return (
                <li key={edge.id} className="px-3 py-2">
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <span className="text-xs text-text-primary">{membershipBasisLabel(edge.basis)}</span>
                    <GradeMark
                      grade={membershipGrade(edge.basis)}
                      source={membershipSourceClass(edge.basis)}
                    />
                  </div>
                  <p className="mt-1 break-all font-mono text-3xs text-text-muted">
                    {membershipBasisDetail(edge.basis)}
                  </p>
                  {href === null ? null : (
                    <a href={href} className="mt-1 inline-block font-mono text-3xs text-accent hover:underline">
                      Open in Loom →
                    </a>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </Panel>

      <Panel legend={`Umbrellas · ${umbrellas.length}`} className="mx-3 mb-3" bodyClassName="p-0">
        {umbrellas.length === 0 ? (
          <p className="p-3 text-3xs text-text-muted">
            {context.umbrellas.authority.state === 'served'
              ? 'No served correlation groups this pull request with another.'
              : context.umbrellas.authority.reason}
          </p>
        ) : (
          <ul className="divide-y divide-edge-subtle">
            {umbrellas.map((umbrella) => (
              <li key={umbrella.id}>
                <button
                  type="button"
                  className="flex w-full flex-wrap items-center justify-between gap-2 px-3 py-2 text-left hover:bg-surface-2"
                  onClick={() => navigate({ mode: 'umbrella', umbrella: umbrella.id })}
                >
                  <span className="min-w-0">
                    <span className="block text-xs text-text-primary">{umbrella.basisLabel}</span>
                    <span className="block truncate font-mono text-3xs text-text-muted">
                      {umbrella.identity} · {umbrella.members.length} PRs · {umbrella.projectIds.length} projects
                    </span>
                  </span>
                  <GradeMark grade={umbrella.grade} source={umbrella.source} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </Panel>

      <div className="mt-auto flex flex-wrap gap-2 border-t border-edge-subtle p-3">
        <ControlLink
          href={compareHref({ branch: row.branch_ref, revision: row.indexed_head_commit_id, file: null })}
        >
          Compare head revision
        </ControlLink>
        {row.shared_code.map((reference) => (
          <ControlLink key={reference.kind} href={reference.href}>
            {reference.kind === 'shared_code' ? 'Open Shared Code' : 'Open Compare'}
          </ControlLink>
        ))}
      </div>
    </section>
  );
}

export function attentionSourceLabel(source: DeliveryAttentionSourceV1): string {
  switch (source) {
    case 'ci_failure':
      return 'CI failure';
    case 'unresolved_review':
      return 'Unresolved review';
    case 'new_review_comment':
      return 'New review comment';
    case 'contradiction':
      return 'Contradiction';
    case 'unsafe_pattern':
      return 'Unsafe pattern';
    case 'test_risk':
      return 'Test risk';
    case 'unreviewed_changed_code':
      return 'Unreviewed changed code';
    case 'weak_evidence':
      return 'Weak evidence';
    case 'evidence_gap':
      return 'Evidence gap';
    case 'overlapping_edit':
      return 'Overlapping edit';
    case 'confirmed_conflict':
      return 'Confirmed conflict';
    case 'divergent_shared_implementation':
      return 'Divergent shared implementation';
    case 'stale_provider_state':
      return 'Stale provider state';
    default: {
      const unhandled: never = source;
      return unhandled;
    }
  }
}

export function attentionStateKind(state: DeliveryAttentionStateV1): DomainStateKind {
  switch (state) {
    case 'active':
      return 'partial';
    case 'clear':
      return 'ready';
    case 'unavailable':
      return 'unavailable';
    case 'denied':
      return 'denied';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

export function pullRequestStateKind(state: DeliveryInboxPullRequestStateV1): DomainStateKind {
  switch (state) {
    case 'current':
      return 'ready';
    case 'partial':
      return 'partial';
    case 'stale':
      return 'stale';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

export function evidenceIdentity(evidence: DeliveryAttentionEvidenceV1): string {
  switch (evidence.kind) {
    case 'provider_operation':
      return `${evidence.operation}:${evidence.fetched_at_micros}`;
    case 'review_comment':
      return `${evidence.comment_id}:${evidence.path}`;
    case 'ci_failure':
      return evidence.failure_anchor;
    case 'indexed_generation':
      return evidence.generation;
    case 'proximity_encounter':
      return `${evidence.encounter_id}:${evidence.relation}`;
    default: {
      const unhandled: never = evidence;
      return unhandled;
    }
  }
}

/** Why-to-code: a review comment names a file, so it can open Compare on it.
 * The other evidence kinds carry no path the Code workspace can honour. */
function evidenceHref(
  evidence: DeliveryAttentionEvidenceV1,
  row: DeliveryInboxPullRequestV1,
): string | null {
  switch (evidence.kind) {
    case 'review_comment':
      return compareHref({
        branch: row.branch_ref,
        revision: row.indexed_head_commit_id,
        file: evidence.path,
      });
    case 'provider_operation':
    case 'ci_failure':
    case 'indexed_generation':
    case 'proximity_encounter':
      return null;
    default: {
      const unhandled: never = evidence;
      return unhandled;
    }
  }
}
