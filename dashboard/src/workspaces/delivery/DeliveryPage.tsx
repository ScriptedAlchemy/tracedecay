import { useMemo } from 'react';
import { AlertTriangle, GitPullRequest, Network } from 'lucide-react';
import { useSearchParams } from 'react-router';
import {
  DeliveryInboxV1Schema,
  type DeliveryAttentionEvidenceV1,
  type DeliveryAttentionSourceV1,
  type DeliveryAttentionStateV1,
  type DeliveryInboxPullRequestV1,
  type DeliveryInboxV1,
  type DeliveryMembershipBasisV1,
  type DeliveryMembershipEdgeV1,
  type DeliveryProviderStateV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { ReadSection, type ReadState } from '../../ui/ReadSection.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Panel, ReadoutBar, WorkspaceHeader } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';

const ATTENTION_SOURCES = [
  'ci_failure',
  'unresolved_review',
  'new_review_comment',
  'contradiction',
  'unsafe_pattern',
  'test_risk',
  'unreviewed_changed_code',
  'weak_evidence',
  'evidence_gap',
  'overlapping_edit',
  'confirmed_conflict',
  'divergent_shared_implementation',
  'stale_provider_state',
] as const satisfies readonly DeliveryAttentionSourceV1[];

export function DeliveryPage() {
  const inbox = useEnvelope(
    ['delivery', 'inbox'],
    '/api/delivery/inbox',
    DeliveryInboxV1Schema,
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <WorkspaceHeader
        path="delivery"
        title="Delivery"
        note="registered projects · indexed PR heads · explicit attention evidence"
      />
      <ReadSection title="Delivery inbox" chrome="centered" state={inboxReadState(inbox.isPending, inbox.data)}>
        {(payload) => <Inbox payload={payload} />}
      </ReadSection>
    </div>
  );
}

function inboxReadState(
  pending: boolean,
  result: EnvelopeResult<DeliveryInboxV1> | undefined,
): ReadState<DeliveryInboxV1> {
  if (pending) {
    return { kind: 'blocked', state: 'loading', detail: 'reading the admitted delivery inbox' };
  }
  if (!result) {
    return { kind: 'blocked', state: 'unknown', detail: 'no inbox response recorded' };
  }
  if (result.outcome === 'transport') {
    return {
      kind: 'blocked',
      state: result.state,
      detail: result.detail ?? 'the delivery inbox could not be read',
    };
  }
  if (result.envelope.authorization.outcome !== 'authorized') {
    return {
      kind: 'blocked',
      state: result.envelope.authorization.outcome,
      detail: 'delivery evidence was not disclosed',
    };
  }
  return { kind: 'ready', value: result.envelope.payload };
}

function Inbox({ payload }: { payload: DeliveryInboxV1 }) {
  const [params, setParams] = useSearchParams();
  const projectFilter = params.get('project');
  const attentionFilter = asAttentionSource(params.get('attention'));
  const evidenceFilter = params.get('evidence');
  const visible = useMemo(
    () =>
      payload.pull_requests.filter((pullRequest) => {
        if (projectFilter !== null && pullRequest.project_id !== projectFilter) return false;
        if (
          attentionFilter !== null &&
          !pullRequest.attention.some((item) => item.source === attentionFilter)
        ) {
          return false;
        }
        return true;
      }),
    [attentionFilter, payload.pull_requests, projectFilter],
  );
  const selectedId = params.get('pr');
  const selected =
    visible.find((pullRequest) => pullRequest.id === selectedId) ?? visible[0] ?? null;

  if (payload.registry_state === 'unavailable') {
    return (
      <CenteredState
        state="unavailable"
        title="Project registry unavailable"
        detail="The daemon could not enumerate registered TraceDecay projects. This is not an empty inbox."
      />
    );
  }

  const providerNotConfigured = payload.projects.some(
    (project) => project.provider_state === 'not_configured',
  );
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ReadoutBar
        label="Inbox readings"
        elevation="raised"
        items={[
          { label: 'projects', value: payload.projects.length },
          { label: 'admitted PRs', value: payload.pull_requests.length },
          {
            label: 'active attention',
            value: payload.pull_requests.reduce(
              (total, pullRequest) =>
                total + pullRequest.attention.filter((item) => item.state === 'active').length,
              0,
            ),
          },
          { label: 'membership edges', value: payload.membership_edges.length },
          { label: 'omitted projects', value: payload.omitted_projects },
          { label: 'excluded provider PRs', value: payload.excluded_pull_requests },
        ]}
      />
      <div className="flex flex-wrap items-end gap-3 border-b border-edge-subtle bg-surface-1 px-3 py-2">
        <label className="flex min-w-48 flex-col gap-1 text-3xs uppercase tracking-wider text-text-muted">
          Project
          <select
            aria-label="Project"
            className="h-9 border border-edge-subtle bg-surface-0 px-2 text-xs normal-case tracking-normal text-text-primary"
            value={projectFilter ?? ''}
            onChange={(event) =>
              updateParams(params, setParams, 'project', event.currentTarget.value)
            }
          >
            <option value="">All registered projects</option>
            {payload.projects.map((project) => (
              <option key={project.project_id} value={project.project_id}>
                {project.label}
              </option>
            ))}
          </select>
        </label>
        <label className="flex min-w-52 flex-col gap-1 text-3xs uppercase tracking-wider text-text-muted">
          Attention source
          <select
            aria-label="Attention source"
            className="h-9 border border-edge-subtle bg-surface-0 px-2 text-xs normal-case tracking-normal text-text-primary"
            value={attentionFilter ?? ''}
            onChange={(event) =>
              updateParams(params, setParams, 'attention', event.currentTarget.value)
            }
          >
            <option value="">All attention sources</option>
            {ATTENTION_SOURCES.map((source) => (
              <option key={source} value={source}>
                {attentionSourceLabel(source)}
              </option>
            ))}
          </select>
        </label>
        {payload.excluded_pull_requests > 0 ? (
          <p className="ml-auto text-3xs text-text-muted">
            {payload.excluded_pull_requests} unrelated provider pull request
            {payload.excluded_pull_requests === 1 ? '' : 's'} excluded
          </p>
        ) : null}
      </div>
      <div className="flex flex-wrap gap-2 border-b border-edge-subtle px-3 py-2">
        {payload.projects.map((project) => (
          <div key={project.project_id} className="flex items-center gap-2 text-3xs">
            <span className="font-medium">{project.label}</span>
            <StateChip
              kind={providerStateKind(project.provider_state)}
              detail={project.provider_state.replaceAll('_', ' ')}
            />
          </div>
        ))}
      </div>
      {visible.length === 0 ? (
        <CenteredState
          state={providerNotConfigured ? 'unavailable' : 'complete_zero_findings'}
          title={providerNotConfigured ? 'Provider not configured' : 'No admitted pull requests'}
          detail={
            providerNotConfigured
              ? 'Registered repositories remain visible, but provider reads are not configured. No unrelated pull request is admitted.'
              : 'The registry and indexed-head join completed with no admitted pull requests.'
          }
          secondary={providerNotConfigured ? 'No admitted pull requests' : undefined}
        />
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-1 overflow-auto lg:grid-cols-[20rem_minmax(0,1fr)] xl:grid-cols-[20rem_minmax(0,1fr)_20rem]">
          <PullRequestQueue
            pullRequests={visible}
            selectedId={selected?.id ?? null}
            onSelect={(pullRequest) => {
              const next = new URLSearchParams(params);
              next.set('project', pullRequest.project_id);
              next.set('pr', pullRequest.id);
              setParams(next);
            }}
          />
          {selected === null ? null : (
            <PullRequestDetail
              pullRequest={selected}
              selectedEvidence={evidenceFilter}
              onEvidence={(evidence) =>
                updateParams(params, setParams, 'evidence', evidence)
              }
            />
          )}
          <MembershipGraph
            edges={payload.membership_edges.filter(
              (edge) => selected !== null && edge.pull_request_id === selected.pull_request.pull_request_id,
            )}
          />
        </div>
      )}
    </div>
  );
}

function PullRequestQueue({
  pullRequests,
  selectedId,
  onSelect,
}: {
  pullRequests: readonly DeliveryInboxPullRequestV1[];
  selectedId: string | null;
  onSelect: (pullRequest: DeliveryInboxPullRequestV1) => void;
}) {
  return (
    <section aria-label="Admitted pull requests" className="border-r border-edge-subtle bg-surface-1">
      <header className="border-b border-edge-subtle px-3 py-2">
        <h2 className="text-xs font-semibold uppercase tracking-wider">Admitted inbox</h2>
        <p className="mt-1 text-3xs text-text-muted">Provider rows appear only after an indexed-head join.</p>
      </header>
      <ul>
        {pullRequests.map((pullRequest) => {
          const active = pullRequest.attention.filter((item) => item.state === 'active').length;
          return (
            <li key={pullRequest.id} className="border-b border-edge-subtle">
              <button
                type="button"
                className={cn(
                  'flex min-h-16 w-full items-start gap-2 px-3 py-2 text-left hover:bg-surface-2',
                  selectedId === pullRequest.id && 'bg-surface-2',
                )}
                onClick={() => onSelect(pullRequest)}
              >
                <GitPullRequest className="mt-0.5 h-4 w-4 shrink-0 text-accent" aria-hidden />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-xs font-medium">
                    {pullRequest.pull_request.identity?.title ?? pullRequest.pull_request.label}
                  </span>
                  <span className="mt-1 block text-3xs text-text-muted">
                    {pullRequest.pull_request.provider} #{pullRequest.pull_request.pull_request_id} ·{' '}
                    {active} active
                  </span>
                </span>
                <StateChip kind={pullRequestStateKind(pullRequest.state)} />
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function PullRequestDetail({
  pullRequest,
  selectedEvidence,
  onEvidence,
}: {
  pullRequest: DeliveryInboxPullRequestV1;
  selectedEvidence: string | null;
  onEvidence: (evidence: string) => void;
}) {
  return (
    <section aria-label="Pull request detail" className="min-w-0 border-r border-edge-subtle">
      <header className="border-b border-edge-subtle px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-sm font-semibold">
            {pullRequest.pull_request.identity?.title ?? pullRequest.pull_request.label}
          </h2>
          <StateChip kind={pullRequestStateKind(pullRequest.state)} />
        </div>
        <p className="mt-1 break-all font-mono text-3xs text-text-muted">
          {pullRequest.branch_ref} · {pullRequest.indexed_head_commit_id.slice(0, 12)} ·{' '}
          {pullRequest.indexed_generation}
        </p>
      </header>
      <div className="grid gap-3 p-3 xl:grid-cols-2">
        {pullRequest.attention.map((item) => (
          <Panel key={item.id} legend={attentionSourceLabel(item.source)}>
            <div className="flex items-center justify-between gap-2">
              <StateChip kind={attentionStateKind(item.state)} detail={item.coverage} />
              <span className="text-3xs text-text-muted">{observationTime(item.observed_at_micros)}</span>
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
                  const identity = evidenceIdentity(evidence);
                  return (
                    <li key={`${identity}:${index}`}>
                      <button
                        type="button"
                        className={cn(
                          'w-full break-all border border-edge-subtle px-2 py-1 text-left font-mono text-3xs',
                          selectedEvidence === identity && 'border-accent bg-surface-2',
                        )}
                        onClick={() => onEvidence(identity)}
                      >
                        {identity}
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </Panel>
        ))}
      </div>
      <div className="flex flex-wrap gap-2 border-t border-edge-subtle p-3">
        {pullRequest.shared_code.map((reference) => (
          <a
            key={reference.kind}
            href={reference.href}
            className="inline-flex min-h-9 items-center border border-edge-strong px-3 text-xs hover:bg-surface-2"
          >
            {reference.kind === 'shared_code' ? 'Open Shared Code' : 'Open Compare'}
          </a>
        ))}
      </div>
    </section>
  );
}

function MembershipGraph({ edges }: { edges: readonly DeliveryMembershipEdgeV1[] }) {
  return (
    <aside
      aria-label="Membership graph"
      className="border-t border-edge-subtle bg-surface-1 lg:col-span-2 xl:col-span-1 xl:border-t-0"
    >
      <header className="border-b border-edge-subtle px-3 py-2">
        <div className="flex items-center gap-2">
          <Network className="h-4 w-4 text-accent" aria-hidden />
          <h2 className="text-xs font-semibold uppercase tracking-wider">Membership graph</h2>
        </div>
        <p className="mt-1 text-3xs text-text-muted">Every edge names the evidence basis.</p>
      </header>
      {edges.length === 0 ? (
        <p className="p-3 text-3xs text-text-muted">No admitted membership edge.</p>
      ) : (
        <ul>
          {edges.map((edge) => (
            <li key={edge.id} className="border-b border-edge-subtle p-3">
              <p className="text-xs font-medium">{membershipBasisLabel(edge.basis)}</p>
              <p className="mt-1 break-all font-mono text-3xs text-text-muted">
                {membershipBasisDetail(edge.basis)}
              </p>
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}

function CenteredState({
  state,
  title,
  detail,
  secondary,
}: {
  state: DomainStateKind;
  title: string;
  detail: string;
  secondary?: string;
}) {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-8">
      <div className="flex max-w-md flex-col items-center gap-3 text-center">
        {state === 'unavailable' ? (
          <AlertTriangle className="h-6 w-6 text-state-offline" aria-hidden />
        ) : null}
        <StateChip kind={state} detail={title} />
        <h2 className="text-sm font-semibold">{title}</h2>
        <p className="text-xs leading-relaxed text-text-muted">{detail}</p>
        {secondary === undefined ? null : <p className="text-xs text-text-secondary">{secondary}</p>}
      </div>
    </div>
  );
}

function updateParams(
  current: URLSearchParams,
  setParams: (next: URLSearchParams) => void,
  key: string,
  value: string,
) {
  const next = new URLSearchParams(current);
  if (value === '') next.delete(key);
  else next.set(key, value);
  setParams(next);
}

function asAttentionSource(value: string | null): DeliveryAttentionSourceV1 | null {
  if (value === null) return null;
  return ATTENTION_SOURCES.find((source) => source === value) ?? null;
}

function attentionSourceLabel(source: DeliveryAttentionSourceV1): string {
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

function attentionStateKind(state: DeliveryAttentionStateV1): DomainStateKind {
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

function pullRequestStateKind(
  state: DeliveryInboxPullRequestV1['state'],
): DomainStateKind {
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

function providerStateKind(state: DeliveryProviderStateV1): DomainStateKind {
  switch (state) {
    case 'ready':
      return 'ready';
    case 'partial':
      return 'partial';
    case 'stale':
      return 'stale';
    case 'rate_limited':
      return 'rate_limited';
    case 'failed':
      return 'error';
    case 'denied':
      return 'denied';
    case 'not_published':
    case 'not_configured':
    case 'unavailable':
      return 'unavailable';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

function membershipBasisLabel(basis: DeliveryMembershipBasisV1): string {
  switch (basis.kind) {
    case 'shared_work_objective':
      return 'Shared Work objective';
    case 'session_git_relation':
      return 'Session and Git relation';
    case 'explicit_handoff':
      return 'Explicit handoff';
    case 'shared_agent':
      return 'Shared agent';
    case 'branch_pull_request_reference':
      return 'Branch and pull request reference';
    default: {
      const unhandled: never = basis;
      return unhandled;
    }
  }
}

function membershipBasisDetail(basis: DeliveryMembershipBasisV1): string {
  switch (basis.kind) {
    case 'shared_work_objective':
      return basis.work_item_id;
    case 'session_git_relation':
      return `${basis.session_id} · ${basis.commit_id}`;
    case 'explicit_handoff':
      return basis.handoff_id;
    case 'shared_agent':
      return basis.agent_id;
    case 'branch_pull_request_reference':
      return `${basis.branch_ref} · ${basis.head_commit_id}`;
    default: {
      const unhandled: never = basis;
      return unhandled;
    }
  }
}

function evidenceIdentity(evidence: DeliveryAttentionEvidenceV1): string {
  switch (evidence.kind) {
    case 'provider_operation':
      return `${evidence.operation}:${evidence.fetched_at_micros}`;
    case 'review_comment':
      return `${evidence.comment_id}:${evidence.path}`;
    case 'ci_failure':
      return evidence.failure_anchor;
    case 'indexed_generation':
      return evidence.generation;
    default: {
      const unhandled: never = evidence;
      return unhandled;
    }
  }
}

function observationTime(observedAtMicros: number | null): string {
  if (observedAtMicros === null) return 'not observed';
  return new Date(Math.floor(observedAtMicros / 1_000)).toISOString();
}
