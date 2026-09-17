import type { UseQueryResult } from '@tanstack/react-query';
import type {
  DeliveryGenerationComparisonV1,
  DeliveryGitHeadV1,
  DeliveryInboxProjectV1,
  DeliveryOverviewV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import {
  ControlLink,
  IdentityRow,
  ProviderStateChip,
  microsToIso,
  shortSha,
} from './deliveryChrome.tsx';
import { providerStateSentence } from './evidence.ts';
import { laneServes, laneStateKind, projectionLaneState, type LaneState } from './journey.ts';
import { laneStateDetail, overviewReadState, ProjectionLedger } from './ProjectionLedger.tsx';

/**
 * The local-first Repositories wing: when the scoped project's provider cannot
 * serve pull requests, Delivery still shows the local Git evidence the daemon
 * did serve — working tree, commits, index freshness — and prints the provider
 * absence with the daemon's own reason. A horizontal band above the inbox, not
 * a page; the only control is the Settings link.
 *
 * The band is drawn from the first render so the provider state is never
 * hidden, but it becomes a named landmark only once the overview read has
 * settled: assistive tech is not pointed at a region whose evidence has not
 * arrived, and the band is marked busy in the meantime.
 */
export function LocalFirstWing({
  project,
  overview,
}: {
  project: DeliveryInboxProjectV1;
  overview: UseQueryResult<EnvelopeResult<DeliveryOverviewV1>>;
}) {
  const state = overviewReadState(overview.isPending, overview.data);
  const reading = state.kind === 'blocked' && state.state === 'loading';
  return (
    <section
      aria-label={reading ? undefined : `Local-first · ${project.label}`}
      aria-busy={reading ? true : undefined}
      className="border-b border-edge-subtle bg-surface-1"
    >
      <header className="flex flex-wrap items-center gap-3 border-b border-edge-subtle px-3 py-2">
        <h2 className="td-title">Repositories wing · local-first</h2>
        <ProviderStateChip state={project.provider_state} />
        <p className="min-w-0 text-3xs text-text-muted">{providerStateSentence(project.provider_state)}</p>
        <div className="ml-auto flex flex-wrap items-center gap-3">
          <ControlLink href="/settings">Open Settings · Provider authority</ControlLink>
          <span className="font-mono text-3xs tracking-[0.08em] text-text-muted">
            Continue with local evidence ↓
          </span>
        </div>
      </header>
      {state.kind === 'blocked' ? (
        <div className="flex items-center px-3 py-4">
          <StateChip kind={state.state} detail={state.detail} />
        </div>
      ) : (
        <>
          <div className="grid grid-cols-1 gap-3 p-3 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_22rem]">
            <WorkingTreePanel projection={state.value.changes} />
            <CommitsPanel projection={state.value.commits} />
            <ProjectionLedger overview={state.value} />
          </div>
          <FreshnessLine projection={state.value.generation_freshness} />
        </>
      )}
    </section>
  );
}

/** The eight projections share one state ladder; only `value` differs. */
function projectionValue<T>(projection: { readonly state: string; readonly value?: T | null }): T | null {
  return projection.value ?? null;
}

function LaneChip({ state, className }: { state: LaneState; className?: string }) {
  return <StateChip kind={laneStateKind(state)} detail={laneStateDetail(state)} className={className} />;
}

function WorkingTreePanel({ projection }: { projection: DeliveryOverviewV1['changes'] }) {
  const state = projectionLaneState(projection, 'Changes');
  const value = laneServes(state) ? projectionValue(projection) : null;
  return (
    <Panel legend="Working tree (local Git)" bodyClassName="p-3">
      {value === null ? (
        <LaneChip state={state} />
      ) : (
        <>
          {state.kind === 'served' ? null : <LaneChip state={state} className="mb-2" />}
          <dl>
            <IdentityRow label="head" value={headValue(value.head)} />
            <IdentityRow label="operation" value={value.operation} />
            <IdentityRow label="staged" value={String(value.staged)} />
            <IdentityRow label="unstaged" value={String(value.unstaged)} />
            <IdentityRow label="untracked" value={String(value.untracked)} />
            <IdentityRow label="conflicted" value={String(value.conflicted)} />
            <IdentityRow label="ignored" value={String(value.ignored)} />
          </dl>
          <p className="td-legend mt-3">changed paths · {value.changed_paths.length}</p>
          {value.changed_paths.length === 0 ? (
            <p className="mt-1 text-3xs text-text-muted">no changed path in the working tree</p>
          ) : (
            <ul className="mt-1 space-y-0.5">
              {value.changed_paths.map((path) => (
                <li key={path} className="break-all font-mono text-3xs text-text-secondary">
                  {path}
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </Panel>
  );
}

function headValue(head: DeliveryGitHeadV1): string {
  switch (head.state) {
    case 'attached':
      return `${head.branch} @ ${shortSha(head.commit, 7)}`;
    case 'detached':
      return `detached @ ${shortSha(head.commit, 7)}`;
    case 'unborn':
      return `${head.branch} · unborn`;
    default: {
      const unhandled: never = head;
      return unhandled;
    }
  }
}

function CommitsPanel({ projection }: { projection: DeliveryOverviewV1['commits'] }) {
  const state = projectionLaneState(projection, 'Commits');
  const value = laneServes(state) ? projectionValue(projection) : null;
  return (
    <Panel
      legend="Commits (local Git)"
      bodyClassName="p-0"
      footer={
        value?.truncated ? (
          <span className="font-mono text-3xs text-text-muted">
            truncated · the daemon served a bounded window of the timeline
          </span>
        ) : undefined
      }
    >
      {value === null || value.items.length === 0 ? (
        <div className="p-3">
          <LaneChip state={state} />
        </div>
      ) : (
        <>
          {state.kind === 'served' ? null : (
            <div className="border-b border-edge-subtle px-3 py-2">
              <LaneChip state={state} />
            </div>
          )}
          <ol className="divide-y divide-edge-subtle">
            {value.items.map((commit) => (
              <li key={commit.commit} className="flex items-start gap-3 px-3 py-2">
                <span className="td-value shrink-0 text-3xs text-accent">{shortSha(commit.commit, 7)}</span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-xs text-text-primary">{commit.subject}</span>
                  <span className="block truncate font-mono text-3xs text-text-muted">
                    {commit.author_name} · {microsToIso(commit.committer_at_micros)}
                  </span>
                </span>
              </li>
            ))}
          </ol>
        </>
      )}
    </Panel>
  );
}

function FreshnessLine({ projection }: { projection: DeliveryOverviewV1['generation_freshness'] }) {
  const state = projectionLaneState(projection, 'Index freshness');
  const value = laneServes(state) ? projectionValue(projection) : null;
  return (
    <footer className="flex flex-wrap items-center gap-3 border-t border-edge-subtle px-3 py-2">
      <span className="td-legend">Index freshness</span>
      {state.kind === 'served' && value !== null ? null : <LaneChip state={state} />}
      {value === null ? null : (
        <>
          <StateChip kind={comparisonKind(value.comparison)} detail={value.comparison} />
          <span className="font-mono text-3xs text-text-muted">
            head {shortSha(value.head_commit)} · indexed {shortSha(value.indexed_commit)}
          </span>
        </>
      )}
    </footer>
  );
}

function comparisonKind(comparison: DeliveryGenerationComparisonV1): DomainStateKind {
  switch (comparison) {
    case 'current':
      return 'ready';
    case 'mismatch':
      return 'stale';
    default: {
      const unhandled: never = comparison;
      return unhandled;
    }
  }
}
