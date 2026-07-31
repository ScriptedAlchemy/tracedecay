import { useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import type { LucideIcon } from 'lucide-react';
import {
  FolderGit2,
  GitBranch,
  GitCommitHorizontal,
  GitFork,
  GitPullRequest,
  Package,
  ScrollText,
  Server,
} from 'lucide-react';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { fetchEnvelope } from '../../data/query/envelope.ts';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { FreshnessMeter } from '../../ui/OpsLayout.tsx';
import { StateChip } from '../../ui/StateChip';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import {
  Legend,
  Meter,
  Panel,
  Readout,
  ReadoutBar,
  WorkspaceHeader,
} from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { formatCount } from '../../ui/format.ts';
import { freshnessTier, relativeAge } from '../../ui/time.ts';
import { useProjectRegistry } from '../../data/query/projectRegistry.ts';
import { DeliveryFieldPlot } from './DeliveryField.tsx';
import { composeDeliveryField, type DeliveryBody, type DeliveryField } from './field.ts';
import {
  type DeliveryCiTimeline,
  type DeliveryCommitTimeline,
  type DeliveryFailureLocalizationTimeline,
  type DeliveryGenerationComparison,
  type DeliveryGenerationFreshness,
  type DeliveryGitStatus,
  type DeliveryHostEvidence,
  type DeliveryOverview,
  DeliveryOverviewSchema,
  type ProjectRepoGroup,
} from '../../contracts/wire.ts';

/**
 * Delivery — the daemon's git surface, read as a field rather than scrolled as
 * a list.
 *
 * `/api/projects` provides the repository field. `/api/delivery/overview`
 * provides bounded active-checkout changes, commit history, and generation
 * comparison plus typed unavailable states for authorities that are not
 * mounted. The page never converts an unavailable projection into an empty
 * timeline.
 *
 * The one word that has to stay exact everywhere on this page: `last_seen_at`
 * is when TraceDecay last INDEXED the checkout, not when anyone last committed
 * to it. Every caption says so, because "recency" on a delivery surface will
 * otherwise be read as commit recency, which would be a fabrication.
 */
export function DeliveryPage() {
  // The shared registry read, not a private copy of it. Under its own key this
  // page fetched the same listing a second time and, because the SSE
  // `project_registry_changed` invalidation names the registry root, was the
  // one reader a rename never reached.
  const projects = useProjectRegistry();
  const overview = useQuery({
    queryKey: ['delivery', 'overview'],
    queryFn: () => fetchEnvelope('/api/delivery/overview', DeliveryOverviewSchema),
  });
  const [selectedId, setSelectedId] = useState<string | null>(null);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <WorkspaceHeader
        path="/delivery"
        title="Delivery"
        note="repositories, changes and commit history · external authorities explicit"
      />
      <LegacyBoundary
        title="Delivery"
        pending={projects.isPending}
        result={projects.data}
      >
        {(data) => {
          if (data.status === 'missing_registry') {
            return (
              <div className="flex min-h-0 flex-1 items-center justify-center p-8">
                <div className="flex max-w-sm flex-col items-center gap-3 text-center">
                  <StateChip kind="unknown" detail="no project registry available" />
                  <p className="text-xs leading-relaxed text-text-muted">
                    The daemon answered without a project registry, so there is
                    no repository to place on the field.{' '}
                    <span className="text-text-secondary">
                      This is the registry reporting itself absent, not an empty
                      workspace.
                    </span>
                  </p>
                </div>
              </div>
            );
          }

          const tree = data.project_tree ?? [];
          return (
            <DeliveryBody_
              tree={tree}
              truncated={data.truncated === true}
              selectedId={selectedId}
              onSelect={setSelectedId}
              overviewPending={overview.isPending}
              overview={overview.data}
            />
          );
        }}
      </LegacyBoundary>
    </div>
  );
}

function DeliveryBody_({
  tree,
  truncated,
  selectedId,
  onSelect,
  overviewPending,
  overview,
}: {
  tree: readonly ProjectRepoGroup[];
  truncated: boolean;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  overviewPending: boolean;
  overview: EnvelopeResult<DeliveryOverview> | undefined;
}) {
  // Keyed on identity: the payload is fetched once and does not churn, and the
  // field's clock only matters at recency-column boundaries.
  const nowSecs = useMemo(() => Math.floor(Date.now() / 1000), [tree]);
  const field = useMemo(() => composeDeliveryField(tree, nowSecs), [tree, nowSecs]);
  const selected =
    field.bodies.find((body) => body.id === selectedId) ?? null;
  const selectedGroup = tree.find(
    (group) => (group.git_common_dir ?? group.label) === selectedId,
  );
  const indexedToday = field.columns[0]?.count ?? 0;

  if (tree.length === 0) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-8">
        <p className="max-w-sm text-center text-xs leading-relaxed text-text-muted">
          The registry answered and holds no repositories in this workspace.{' '}
          <span className="text-text-secondary">
            Repositories appear here once TraceDecay has indexed one.
          </span>
        </p>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ReadoutBar
        label="Registry readings"
        elevation="raised"
        items={[
          { label: 'repositories', value: field.bodies.length },
          {
            label: 'branches',
            value: formatCount(field.totalBranches),
            note: `across ${field.bodies.length - field.unknownBranchCount} git repos`,
          },
          { label: 'checkouts', value: field.totalCheckouts },
          {
            label: 'worktrees',
            value: field.totalWorktrees,
            note: field.totalWorktrees === 0 ? 'none registered' : undefined,
          },
          {
            label: 'indexed today',
            value: indexedToday,
            fraction:
              field.bodies.length > 0 ? indexedToday / field.bodies.length : null,
            note: 'of all repositories',
          },
          {
            label: 'busiest repo',
            value: formatCount(field.branchCeiling),
            note: 'branches',
          },
        ]}
      />

      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-3 [scrollbar-gutter:stable] xl:flex-row">
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <DeliveryFieldPlot
            field={field}
            selectedId={selectedId}
            onSelect={onSelect}
            ariaLabel={fieldDescription(field)}
          />
          <FieldAxis field={field} />
          <RepoTable
            field={field}
            selectedId={selectedId}
            onSelect={onSelect}
            truncated={truncated}
            nowSecs={nowSecs}
          />
        </div>

        <aside className="flex w-full shrink-0 flex-col gap-3 xl:w-[22rem]">
          <Panel legend="Pipeline">
            <PipelineOverview pending={overviewPending} result={overview} />
          </Panel>

          <RepoDetail body={selected} group={selectedGroup} nowSecs={nowSecs} />
        </aside>
      </div>
    </div>
  );
}

function PipelineOverview({
  pending,
  result,
}: {
  pending: boolean;
  result: EnvelopeResult<DeliveryOverview> | undefined;
}) {
  if (pending) {
    return <StateChip kind="loading" detail="reading delivery projections" />;
  }
  if (!result) {
    return <StateChip kind="error" detail="delivery overview read failed" />;
  }
  if (result.outcome === 'transport') {
    return (
      <StateChip
        kind={result.state}
        detail={result.detail ?? `delivery overview ${result.state.replaceAll('_', ' ')}`}
      />
    );
  }
  const payload = result.envelope.payload;
  const changesDetail =
    payload.changes.state === 'ready' && payload.commits.state === 'ready'
      ? `${commitCountDetail(payload.commits.value)} · ${countLabel(payload.changes.value.changed_paths.length, 'changed path')}`
      : null;
  const pullRequestDetail =
    payload.pull_requests.state === 'ready' && payload.review_comments.state === 'ready'
      ? `${payload.pull_requests.value.items.length} pull requests · ${payload.review_comments.value.items.length} review comments`
      : null;
  const ciDetail =
    payload.ci_checks.state === 'ready' && payload.failure_localization.state === 'ready'
      ? `${payload.ci_checks.value.items.length} checks · ${payload.failure_localization.value.items.length} localized failures`
      : null;

  return (
    <div className="flex flex-col gap-2.5">
      <p className="text-2xs leading-relaxed text-text-muted">
        Active-checkout Git reads are bounded and live. External provider
        projections stay explicitly unavailable until their read authorities
        are mounted.
      </p>
      <PipelineStage icon={GitCommitHorizontal} label="Changes & commits">
        {changesDetail ? (
          <StateChip kind="ready" detail={changesDetail} />
        ) : (
          <ProjectionState projection={firstMissing(payload.changes, payload.commits)} />
        )}
      </PipelineStage>
      <PipelineStage icon={GitPullRequest} label="Pull requests & review">
        {pullRequestDetail ? (
          <StateChip kind="ready" detail={pullRequestDetail} />
        ) : (
          <ProjectionState
            projection={firstMissing(payload.pull_requests, payload.review_comments)}
          />
        )}
      </PipelineStage>
      <PipelineStage icon={Server} label="Continuous integration">
        {ciDetail ? (
          <StateChip kind="ready" detail={ciDetail} />
        ) : (
          <ProjectionState
            projection={firstMissing(payload.ci_checks, payload.failure_localization)}
          />
        )}
      </PipelineStage>
      <PipelineStage icon={Package} label="Releases">
        {payload.releases.state === 'ready' ? (
          <StateChip
            kind="ready"
            detail={`${payload.releases.value.items.length} releases`}
          />
        ) : (
          <ProjectionState projection={payload.releases} />
        )}
      </PipelineStage>
      <PipelineStage icon={ScrollText} label="Index freshness">
        {payload.generation_freshness.state === 'ready' ? (
          <div className="flex flex-wrap items-center gap-1.5">
            <StateChip
              kind={payload.generation_freshness.value.comparison === 'current' ? 'ready' : 'stale'}
              detail={`${payload.generation_freshness.value.comparison} · HEAD ${shortOid(payload.generation_freshness.value.head_commit)} · indexed ${shortOid(payload.generation_freshness.value.indexed_commit)}`}
            />
            <EvidencePattern
              quality={
                payload.generation_freshness.value.comparison === 'current'
                  ? 'measured'
                  : 'unknown'
              }
            />
          </div>
        ) : (
          <div className="flex flex-wrap items-center gap-1.5">
            <ProjectionState projection={payload.generation_freshness} />
            <EvidencePattern quality="unknown" />
          </div>
        )}
      </PipelineStage>
    </div>
  );
}

type ProjectionStateValue = {
  state?: 'ready' | 'unavailable' | 'unsupported';
  reason?: string;
  required_authority?: string;
};

function firstMissing(
  first: ProjectionStateValue,
  second: ProjectionStateValue,
): ProjectionStateValue {
  return first.state === 'ready' ? second : first;
}

function ProjectionState({ projection }: { projection: ProjectionStateValue }) {
  switch (projection.state) {
    case 'ready':
      return <StateChip kind="ready" detail="projection ready" />;
    case 'unavailable':
      return (
        <StateChip
          kind="unknown"
          detail={`unavailable · ${projection.reason ?? projection.required_authority ?? 'authority absent'}`}
        />
      );
    case 'unsupported':
      return (
        <StateChip
          kind="unsupported"
          detail={projection.reason ?? projection.required_authority ?? 'source unsupported'}
        />
      );
    case undefined:
      return <StateChip kind="unsupported_schema" detail="projection state is missing" />;
    default: {
      const exhaustive: never = projection.state;
      return <StateChip kind="unsupported_schema" detail={String(exhaustive)} />;
    }
  }
}

/**
 * One stage's state, worst first, and whatever its value-bearing sources did
 * measure. `Ready` is drawn only when every reading in the stage is ready — a
 * degradation any one of them reported can never be painted over by its
 * partner's success.
 */
function SourceStates({
  readings,
  measured,
}: {
  readings: readonly SourceReading[];
  measured: string | null;
}) {
  const degraded = collapseByFinding(readings.filter((reading) => reading.kind !== 'ready')).sort(
    (a, b) => b.severity - a.severity,
  );
  if (degraded.length === 0) {
    return <StateChip kind="ready" detail={measured ?? 'read complete'} />;
  }
  return (
    <div className="flex flex-col items-start gap-1">
      {degraded.map((reading) => (
        <StateChip
          key={`${reading.source}:${reading.kind}:${reading.detail}`}
          kind={reading.kind}
          detail={`${reading.source} · ${reading.detail}`}
        />
      ))}
      {measured ? (
        <span className="text-3xs leading-relaxed text-text-muted">
          Retained by this read: {measured}
        </span>
      ) : null}
    </div>
  );
}

/**
 * One chip per distinct finding, not per source.
 *
 * The two sources a stage covers routinely share one authority, so they report
 * the same state for the same reason — and the same sentence twice on one panel
 * reads as two separate findings. Naming both sources on one chip keeps every
 * source accounted for while saying the reason once.
 */
function collapseByFinding(readings: readonly SourceReading[]): SourceReading[] {
  const byFinding = new Map<string, SourceReading>();
  for (const reading of readings) {
    const finding = `${reading.kind}\u0000${reading.detail}`;
    const seen = byFinding.get(finding);
    byFinding.set(
      finding,
      seen === undefined ? reading : { ...seen, source: `${seen.source} and ${reading.source}` },
    );
  }
  return [...byFinding.values()];
}

/**
 * A CI check's own source degradation, which its projection state does not
 * carry.
 *
 * The archive can return a complete, validated record whose provider read was
 * rate limited or failed underneath it. `ci_checks` is then legitimately
 * `ready`, and drawing that as a green Ready said the run was fully read when it
 * was not. Rate limiting and failure stay distinct: one is a read that will
 * complete later, the other is a read that did not happen.
 */
const CI_SOURCE_DEGRADATION: Record<
  string,
  { readonly kind: DomainStateKind; readonly severity: number; readonly note: string }
> = {
  rate_limited: {
    kind: 'partial',
    severity: 2,
    note: 'the provider read was rate limited, so this run is not fully read',
  },
  failed: { kind: 'error', severity: 5, note: 'the provider read failed for this run' },
};

function ciSourceDegradations(checks: DeliveryCiTimeline | null): SourceReading[] {
  if (checks === null) return [];
  return checks.items.flatMap((check) => {
    if (check.source_degradation === null || check.source_degradation === '') return [];
    const source = `${check.provider} provider read`;
    const known = CI_SOURCE_DEGRADATION[check.source_degradation];
    return [
      known === undefined
        ? {
            source,
            kind: 'unknown' as const,
            severity: 6,
            // Named, not swallowed: an unrecognized degradation label is still
            // the daemon saying the read was degraded.
            detail: `reported source degradation ${check.source_degradation}, which this build does not recognize`,
          }
        : { source, kind: known.kind, severity: known.severity, detail: known.note },
    ];
  });
}

/** The indexed generation as its own reading. A `ready` projection can carry a
 * `behind` comparison, which is a stale index and not a complete read. */
const GENERATION_STATE: Record<DeliveryGenerationComparison, DomainStateKind> = {
  current: 'ready',
  behind: 'stale',
};

const GENERATION_EVIDENCE: Record<DeliveryGenerationComparison, EvidenceQuality> = {
  current: 'measured',
  behind: 'unknown',
};

function generationComparison(
  generation: DeliveryGenerationFreshness | null,
): SourceReading[] {
  if (generation === null || GENERATION_STATE[generation.comparison] === 'ready') return [];
  return [
    {
      source: 'indexed commit',
      kind: GENERATION_STATE[generation.comparison],
      severity: 1,
      detail: `${generation.comparison} · HEAD ${shortOid(generation.head_commit)} · indexed ${shortOid(generation.indexed_commit)}`,
    },
  ];
}

function generationEvidence(generation: DeliveryGenerationFreshness | null): EvidenceQuality {
  return generation === null ? 'unknown' : GENERATION_EVIDENCE[generation.comparison];
}

/* --------------------------------------------------------- measured detail */

function changeDetail(
  changes: DeliveryGitStatus | null,
  commits: DeliveryCommitTimeline | null,
): string | null {
  const parts: string[] = [];
  if (commits !== null) parts.push(commitCountDetail(commits));
  if (changes !== null) {
    parts.push(countLabel(changes.changed_paths.length, 'changed path'));
  }
  return parts.length === 0 ? null : parts.join(' · ');
}

function reviewDetail(
  pullRequests: DeliveryPullRequestTimeline | null,
  reviews: DeliveryReviewTimeline | null,
): string | null {
  const parts: string[] = [];
  if (pullRequests !== null) {
    parts.push(countLabel(pullRequests.items.length, 'pull request'));
  }
  if (reviews !== null) parts.push(countLabel(reviews.items.length, 'review comment'));
  return parts.length === 0 ? null : parts.join(' · ');
}

function ciDetail(
  checks: DeliveryCiTimeline | null,
  localization: DeliveryFailureLocalizationTimeline | null,
): string | null {
  const parts: string[] = [];
  if (checks !== null) parts.push(countLabel(checks.items.length, 'check'));
  if (localization !== null) {
    parts.push(countLabel(localization.items.length, 'localized failure'));
  }
  return parts.length === 0 ? null : parts.join(' · ');
}

/**
 * Releases read from a bounded tag walk, stated as a floor.
 *
 * The commit timeline carries a `truncated` flag and this projection does not,
 * so nothing here can tell a repository that has exactly this many tags from one
 * whose walk hit its bound. A bare count would let the second read as the first,
 * so the missing statement is disclosed rather than papered over.
 */
function releaseDetail(releases: DeliveryReleaseTimeline | null): string | null {
  return releases === null
    ? null
    : `${countLabel(releases.items.length, 'release')} · no truncation stated, so this is a floor`;
}

function generationDetail(generation: DeliveryGenerationFreshness | null): string | null {
  return generation === null
    ? null
    : `HEAD ${shortOid(generation.head_commit)} · indexed ${shortOid(generation.indexed_commit)}`;
}

/* ------------------------------------------------------------- host rows */

/**
 * The two independently measured axes of one host component: whether the
 * receipt-backed component itself is current, and whether its registration is.
 * A registration that is missing while the component is current is a real,
 * separately reportable degradation, so each gets its own chip rather than being
 * folded into one word.
 */
const HOST_COMPONENT_STATE: Record<string, DomainStateKind> = {
  current: 'ready',
  // Measured with no doctor verdict attached — neither graded healthy nor
  // reported degraded, which is exactly what `unknown` says.
  observed: 'unknown',
  repairable: 'partial',
  ownership_conflict: 'conflicting',
  missing: 'unavailable',
  corrupt: 'error',
};

const HOST_REGISTRATION_STATE: Record<string, DomainStateKind> = {
  current: 'ready',
  repairable: 'partial',
  missing: 'unavailable',
  corrupt: 'error',
};

const DEGRADED_HOST_STATES: ReadonlySet<DomainStateKind> = new Set<DomainStateKind>([
  'partial',
  'conflicting',
  'unavailable',
  'error',
]);

function HostEvidenceItems({ projection }: { projection: DeliveryOverview['host_evidence'] }) {
  if (!('value' in projection)) return null;
  const rows = projection.value.items.map(hostRow);
  const degraded = rows.filter((row) => row.degraded).length;
  const unreadable = rows.filter((row) => row.unreadable).length;
  const artifacts = rows.reduce((total, row) => total + row.artifacts, 0);
  return (
    <div className="flex flex-col gap-1">
      <p className="text-3xs leading-relaxed text-text-muted">
        {projection.state === 'ready' ? '' : 'Retained by this read: '}
        {countLabel(rows.length, 'component row')} · {degraded} degraded ·{' '}
        {countLabel(artifacts, 'artifact')}
        {unreadable > 0 ? ` · ${unreadable} in a state this build cannot read` : ''}
      </p>
      <ul aria-label="Host integration components" className="flex flex-col gap-1">
        {rows.map((row) => (
          <li key={row.key}>
            <div
              role="group"
              aria-label={row.name}
              className="flex flex-wrap items-center gap-x-1.5 gap-y-1"
            >
              <span className="text-2xs text-text-secondary">{row.name}</span>
              <StateChip kind={row.component.kind} detail={row.component.detail} />
              {row.registration === null ? null : (
                <StateChip kind={row.registration.kind} detail={row.registration.detail} />
              )}
              <span className="text-3xs text-text-muted">
                {countLabel(row.artifacts, 'artifact')}
              </span>
              {/* The receipt path is longer than this rail, and the path is the
                * evidence — so the truncated view carries the whole thing on
                * hover rather than leaving the reader with a stem. */}
              <span
                className="td-value min-w-0 truncate text-3xs text-text-muted"
                title={row.evidenceSource}
              >
                {row.evidenceSource}
              </span>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

interface HostAxis {
  readonly kind: DomainStateKind;
  readonly detail: string;
}

interface HostRow {
  readonly key: string;
  readonly name: string;
  readonly component: HostAxis;
  readonly registration: HostAxis | null;
  readonly artifacts: number;
  readonly evidenceSource: string;
  readonly degraded: boolean;
  readonly unreadable: boolean;
}

function hostRow(item: DeliveryHostEvidence): HostRow {
  const component: HostAxis = {
    kind: HOST_COMPONENT_STATE[item.state] ?? 'unsupported_schema',
    detail: `component ${readable(item.state)}`,
  };
  const registration: HostAxis | null =
    item.registration === null
      ? null
      : {
          kind: HOST_REGISTRATION_STATE[item.registration] ?? 'unsupported_schema',
          detail: `registration ${readable(item.registration)}`,
        };
  return {
    key: `${item.host}:${item.component ?? 'bundle'}:${item.evidence_source}`,
    name: `${item.host} · ${item.component ?? 'bundle'}`,
    component,
    registration,
    artifacts: item.artifact_count,
    evidenceSource: item.evidence_source,
    degraded:
      DEGRADED_HOST_STATES.has(component.kind)
      || (registration !== null && DEGRADED_HOST_STATES.has(registration.kind)),
    // Counted apart from `degraded` so an unrecognized label is neither claimed
    // as healthy nor silently folded into a degradation total.
    unreadable:
      component.kind === 'unsupported_schema' || registration?.kind === 'unsupported_schema',
  };
}

function readable(value: string): string {
  return value.replaceAll('_', ' ');
}

function shortOid(value: string): string {
  return value.slice(0, 8);
}

function countLabel(count: number, singular: string): string {
  return `${count} ${singular}${count === 1 ? '' : 's'}`;
}

function commitCountDetail(commits: {
  items?: readonly unknown[];
  truncated?: boolean;
}): string {
  if (!commits.items || commits.truncated == null) {
    return 'commit projection incomplete';
  }
  const count = countLabel(commits.items.length, 'commit');
  return commits.truncated ? `${count} shown · more commits not shown` : count;
}

/** The axes, printed. Both of them are easy to misread — one looks like commit
 * recency and is not, the other is logarithmic — so both are stated. */
function FieldAxis({ field }: { field: DeliveryField }) {
  const busiest = field.columns.reduce((max, column) => Math.max(max, column.count), 0);
  return (
    <div className="flex flex-col gap-1.5">
      <Legend>last indexed across · branches up</Legend>
      <div className="flex flex-wrap border-y border-edge-subtle bg-surface-1">
        {field.columns.map((column) => (
          <div
            key={column.id}
            className="min-w-0 flex-1 basis-24 border-l border-edge-subtle px-2.5 py-1.5 first:border-l-0"
          >
            <Readout
              label={column.label}
              value={column.count}
              unit={column.bound}
              fraction={busiest > 0 ? column.count / busiest : null}
              size="sm"
            />
          </div>
        ))}
      </div>
      <p className="text-2xs leading-relaxed text-text-muted">
        Each body is one repository: column = when TraceDecay last indexed it —{' '}
        <span className="text-text-secondary">
          not when it was last committed to; commit history is shown separately
          for the active checkout
        </span>{' '}
        — height = how many branches it has ({field.branchFloor} to{' '}
        {field.branchCeiling}, log scale), size = how many checkouts map to it,
        brightness = the same index recency. A ring marks the active project.{' '}
        {field.unknownBranchCount > 0
          ? `${field.unknownBranchCount} ${field.unknownBranchCount === 1 ? 'entry sits' : 'entries sit'} in the fenced band below the plot: they have no git directory, so their branch count is unknown rather than zero. `
          : ''}
        {field.multiCheckoutCount === 0
          ? 'Every repository here has exactly one checkout, so every body is the same size — the size channel is live but this registry has nothing to spend it on.'
          : `${field.multiCheckoutCount} ${field.multiCheckoutCount === 1 ? 'repository has' : 'repositories have'} more than one checkout and ${field.multiCheckoutCount === 1 ? 'is' : 'are'} drawn larger.`}
      </p>
    </div>
  );
}

/** The field's accessible equivalent, and the scanning surface: one line per
 * repository instead of the header-plus-row pair the flat list used to spend
 * on each one. */
function RepoTable({
  field,
  selectedId,
  onSelect,
  truncated,
  nowSecs,
}: {
  field: DeliveryField;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  truncated: boolean;
  nowSecs: number;
}) {
  return (
    <section aria-label="Repositories" className="flex min-w-0 flex-col">
      <Legend>repositories · most recently indexed first</Legend>
      <div
        role="region"
        aria-label="Repositories table"
        className="mt-1.5 max-h-80 overflow-auto border border-edge-subtle"
      >
        <table className="w-full border-collapse text-2xs">
          <caption className="sr-only">
            Every repository on the field, most recently indexed first, with its
            branch count, checkouts and default branch.
          </caption>
          <thead className="sticky top-0 bg-surface-2">
            <tr className="text-left text-text-secondary">
              <th scope="col" className="px-2 py-1 font-medium">Repository</th>
              <th scope="col" className="px-2 py-1 font-medium">Default branch</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Branches</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Checkouts</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Indexed</th>
            </tr>
          </thead>
          <tbody>
            {field.bodies.map((body) => (
              <tr
                key={body.id}
                className={cn(
                  'border-t border-edge-subtle',
                  selectedId === body.id && 'bg-accent/10',
                )}
              >
                <td className="max-w-0 px-2 py-1">
                  <button
                    type="button"
                    onClick={() => onSelect(selectedId === body.id ? null : body.id)}
                    aria-pressed={selectedId === body.id}
                    className="flex min-h-[var(--touch-target-min)] w-full min-w-0 items-center gap-1.5 text-left"
                  >
                    {body.branches == null ? (
                      <FolderGit2 aria-hidden size={11} className="shrink-0 text-text-muted" />
                    ) : (
                      <GitBranch aria-hidden size={11} className="shrink-0 text-text-muted" />
                    )}
                    <span className="truncate text-text-primary">{body.label}</span>
                    {body.active ? (
                      <span className="td-legend shrink-0 bg-accent/15 px-1 text-text-primary">
                        active
                      </span>
                    ) : null}
                  </button>
                </td>
                <td className="max-w-0 truncate px-2 py-1 text-text-secondary">
                  {body.defaultBranch ?? '—'}
                </td>
                <td className="px-2 py-1 text-right" data-cell="numeric">
                  {body.branches == null ? (
                    <span className="text-text-muted">unknown</span>
                  ) : (
                    <span className="inline-flex items-center gap-1.5">
                      <Meter
                        fraction={
                          field.branchCeiling > 0
                            ? body.branches / field.branchCeiling
                            : null
                        }
                        className="w-10 shrink-0"
                      />
                      <span className="tabular-nums text-text-primary">
                        {body.branches}
                      </span>
                    </span>
                  )}
                </td>
                <td
                  className="px-2 py-1 text-right text-text-secondary tabular-nums"
                  data-cell="numeric"
                >
                  {body.checkouts}
                  {body.worktrees > 0 ? (
                    <span className="text-text-muted"> ({body.worktrees} wt)</span>
                  ) : null}
                </td>
                <td
                  className="px-2 py-1 text-right text-text-muted tabular-nums"
                  data-cell="numeric"
                >
                  <FreshnessMeter
                    tier={freshnessTier(Math.max(0, nowSecs - body.lastSeenAt))}
                    label={relativeAge(body.lastSeenAt, nowSecs) ?? 'not recorded'}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {truncated ? (
        <p className="pt-1 text-2xs text-text-muted">
          Result truncated by the registry limit — more repositories exist than
          are drawn.
        </p>
      ) : null}
    </section>
  );
}

/** The selected repository, expanded: its checkouts and the branch names the
 * registry actually holds. Branch names are all there is — no branch here
 * carries a time, an author or a tip commit. */
function RepoDetail({
  body,
  group,
  nowSecs,
}: {
  body: DeliveryBody | null;
  group: ProjectRepoGroup | undefined;
  nowSecs: number;
}) {
  if (!body || !group) {
    return (
      <Panel legend="Repository">
        <p className="text-2xs leading-relaxed text-text-muted">
          Select a repository — on the field or in the table — to see its
          checkouts and the branch names the registry holds for it.
        </p>
      </Panel>
    );
  }
  return (
    <Panel legend="Repository">
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-1">
          <span className="text-xs font-medium leading-snug text-text-primary">
            {body.label}
          </span>
          <span className="td-value truncate text-3xs text-text-muted">
            {group.git_common_dir ?? 'no git directory'}
          </span>
        </div>

        <dl className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-2xs">
          <Fact
            label="branches"
            value={body.branches == null ? 'unknown' : String(body.branches)}
            muted={body.branches == null}
          />
          <Fact label="checkouts" value={String(body.checkouts)} />
          <Fact label="worktrees" value={String(body.worktrees)} />
          <Fact
            label="last indexed"
            value={relativeAge(body.lastSeenAt, nowSecs) ?? 'not recorded'}
          />
        </dl>

        <div className="flex flex-col gap-1.5">
          <Legend>checkouts</Legend>
          <ul className="flex flex-col border border-edge-subtle">
            {group.projects.map((project) => (
              <li
                key={project.project_id}
                className="flex items-center gap-1.5 border-b border-edge-subtle px-2 py-1 last:border-b-0"
              >
                {project.kind === 'worktree' ? (
                  <GitFork aria-hidden size={11} className="shrink-0 text-text-muted" />
                ) : (
                  <FolderGit2 aria-hidden size={11} className="shrink-0 text-text-muted" />
                )}
                <span className="td-value min-w-0 flex-1 truncate text-3xs text-text-secondary">
                  {project.project_root}
                </span>
                <span className="td-legend shrink-0 text-text-muted">
                  {project.kind}
                </span>
              </li>
            ))}
          </ul>
        </div>

        <div className="flex flex-col gap-1.5">
          <Legend
            trailing={
              <span
                className="td-value shrink-0 text-3xs text-text-muted"
                data-cell="numeric"
              >
                {group.branches.length}
              </span>
            }
          >
            branch names
          </Legend>
          {group.branches.length === 0 ? (
            <StateChip
              kind={body.branches == null ? 'unsupported' : 'complete_zero_findings'}
              detail={
                body.branches == null
                  ? 'not a git checkout'
                  : 'registry holds no branch names'
              }
            />
          ) : (
            <>
              <ul className="max-h-40 overflow-auto border border-edge-subtle">
                {group.branches.map((branch) => (
                  <li
                    key={branch}
                    className="td-value truncate border-b border-edge-subtle px-2 py-0.5 text-3xs text-text-secondary last:border-b-0"
                  >
                    {branch}
                  </li>
                ))}
              </ul>
              <span className="text-3xs leading-relaxed text-text-muted">
                Names only. The registry records no tip commit, author or time
                for any of these.
              </span>
            </>
          )}
        </div>
      </div>
    </Panel>
  );
}

function fieldDescription(field: DeliveryField): string {
  const occupied = field.columns
    .filter((column) => column.count > 0)
    .map((column) => `${column.count} ${column.label}`)
    .join(', ');
  return `Delivery field: ${field.bodies.length} repositories placed by when TraceDecay last indexed them (${occupied || 'none'}) and by branch count, ${field.branchFloor} to ${field.branchCeiling}. ${field.unknownBranchCount} entries have no git directory and no branch measurement. The repository table below is the accessible equivalent.`;
}

function PipelineStage({
  icon: Icon,
  label,
  children,
}: {
  icon: LucideIcon;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <span className="flex items-center gap-1.5 text-2xs font-medium text-text-secondary">
        <Icon aria-hidden size={12} className="text-text-muted" />
        {label}
      </span>
      {children}
    </div>
  );
}

function Fact({
  label,
  value,
  muted,
}: {
  label: string;
  value: string;
  muted?: boolean;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="td-legend">{label}</dt>
      <dd
        className={
          muted
            ? 'truncate text-3xs text-text-muted'
            : 'truncate text-3xs text-text-secondary'
        }
      >
        {value}
      </dd>
    </div>
  );
}
