import type {
  DeliveryAgentUsageRowV1,
  DeliveryAgentUsageV1,
  DeliveryCiCheckV1,
  DeliveryCommitV1,
  DeliveryGitHubQuarantinedCommentV1,
  DeliveryGitHubQuarantineReasonV1,
  DeliveryInboxPullRequestV1,
  DeliveryMembershipEdgeV1,
  DeliveryOverviewV1,
  DeliveryPullRequestV1,
  DeliveryReleaseV1,
  DeliveryReviewItemV1,
  DeliveryReviewObservationV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import {
  membershipGrade,
  membershipHref,
  membershipSourceClass,
  type EvidenceGrade,
  type SourceClass,
} from './evidence.ts';
import { checkStatusKind, compareHref, latestObservation } from './review.ts';

/**
 * The PR journey model: the episodes Delivery can really join today, one lane
 * per source authority. Every episode carries its source class, its evidence
 * grade and, when the record has no timestamp of its own, `undated` rather
 * than an invented time. Observation time (when the daemon read the provider)
 * is labelled as such and never presented as event time.
 */
export const JOURNEY_LANES = [
  'objective',
  'sessions',
  'agents',
  'commits',
  'pull_request',
  'reviews',
  'checks',
  'releases',
] as const;

export type JourneyLaneId = (typeof JOURNEY_LANES)[number];

export type LaneState =
  | { readonly kind: 'served'; readonly detail?: string }
  | { readonly kind: 'served_empty'; readonly detail: string }
  | { readonly kind: 'stale'; readonly detail: string }
  | { readonly kind: 'partial'; readonly detail: string }
  | { readonly kind: 'rate_limited'; readonly detail: string }
  | { readonly kind: 'failed'; readonly detail: string }
  | { readonly kind: 'denied'; readonly detail: string }
  | { readonly kind: 'not_published'; readonly detail: string; readonly requiredAuthority: string }
  | { readonly kind: 'unavailable'; readonly detail: string; readonly requiredAuthority?: string };

export type EpisodeRef =
  | { readonly kind: 'work_objective'; readonly workItemId: string }
  | { readonly kind: 'session'; readonly sessionId: string; readonly commitId: string }
  | { readonly kind: 'agent'; readonly agentId: string }
  | { readonly kind: 'agent_usage'; readonly branch: string; readonly usage: DeliveryAgentUsageRowV1 }
  | { readonly kind: 'handoff'; readonly handoffId: string }
  | { readonly kind: 'commit'; readonly commit: DeliveryCommitV1 }
  | { readonly kind: 'pull_request'; readonly pullRequest: DeliveryPullRequestV1 }
  | {
      readonly kind: 'provider_observation';
      readonly operation: string;
      readonly outcome: string;
      readonly fetchedAtMicros: number;
    }
  | { readonly kind: 'review'; readonly item: DeliveryReviewItemV1; readonly observation: DeliveryReviewObservationV1 }
  | { readonly kind: 'check'; readonly check: DeliveryCiCheckV1 }
  | { readonly kind: 'release'; readonly release: DeliveryReleaseV1 };

export interface JourneyEpisode {
  readonly id: string;
  readonly lane: JourneyLaneId;
  readonly label: string;
  readonly detail: string;
  readonly source: SourceClass;
  readonly grade: EvidenceGrade;
  /** Recorded microseconds, or `null` for an undated record. */
  readonly at: number | null;
  /** Whether `at` is the event's own time or the daemon's observation time. */
  readonly timeKind: 'event' | 'observed' | 'undated';
  readonly status: DomainStateKind | null;
  /** A real why-to-code / why-to-source destination, or `null`. */
  readonly href: string | null;
  readonly ref: EpisodeRef;
}

export interface JourneyLane {
  readonly id: JourneyLaneId;
  readonly label: string;
  readonly source: SourceClass;
  readonly state: LaneState;
  readonly episodes: readonly JourneyEpisode[];
}

export interface JourneyModel {
  readonly lanes: readonly JourneyLane[];
  readonly episodes: readonly JourneyEpisode[];
  readonly span: { readonly start: number; readonly end: number } | null;
  readonly undated: number;
  /** Typed gaps that are not a lane's own state: e.g. the selected PR missing
   * from the head-bound provider page. */
  readonly gaps: readonly string[];
}

/** The projections share one state ladder; only `value` differs. */
type AnyProjection = DeliveryOverviewV1[keyof DeliveryOverviewV1];

export function projectionLaneState(projection: AnyProjection, source: string): LaneState {
  switch (projection.state) {
    case 'ready':
      return { kind: 'served' };
    case 'empty_measured':
      return { kind: 'served_empty', detail: `${source} measured and served zero items` };
    case 'stale':
      return { kind: 'stale', detail: `${source} is retained from an earlier head` };
    case 'partial':
      return { kind: 'partial', detail: `${source} was truncated or answered short` };
    case 'rate_limited':
      return {
        kind: 'rate_limited',
        detail:
          projection.retry_at_micros === null
            ? `${source} paused by provider quota`
            : `${source} paused by provider quota · retry ${new Date(
                Math.floor(projection.retry_at_micros / 1000),
              ).toISOString()}`,
      };
    case 'failed':
      return { kind: 'failed', detail: `${source} read failed` };
    case 'denied':
      return { kind: 'denied', detail: `${source} refused for this identity` };
    case 'not_published':
      return {
        kind: 'not_published',
        detail: projection.reason,
        requiredAuthority: projection.required_authority,
      };
    case 'unavailable':
      return {
        kind: 'unavailable',
        detail: projection.reason,
        requiredAuthority: projection.required_authority,
      };
    default: {
      const unhandled: never = projection;
      return unhandled;
    }
  }
}

export function laneStateKind(state: LaneState): DomainStateKind {
  switch (state.kind) {
    case 'served':
      return 'ready';
    case 'served_empty':
      return 'complete_zero_findings';
    case 'stale':
      return 'stale';
    case 'partial':
      return 'partial';
    case 'rate_limited':
      return 'rate_limited';
    case 'failed':
      return 'error';
    case 'denied':
      return 'denied';
    case 'not_published':
    case 'unavailable':
      return 'unavailable';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

/** Whether the lane's authority produced rows a reader may trust as current. */
export function laneServes(state: LaneState): boolean {
  switch (state.kind) {
    case 'served':
    case 'served_empty':
    case 'stale':
    case 'partial':
    case 'rate_limited':
      return true;
    case 'failed':
    case 'denied':
    case 'not_published':
    case 'unavailable':
      return false;
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

export function laneLabel(lane: JourneyLaneId): string {
  switch (lane) {
    case 'objective':
      return 'Objective';
    case 'sessions':
      return 'Sessions';
    case 'agents':
      return 'Agents';
    case 'commits':
      return 'Commits';
    case 'pull_request':
      return 'Pull request';
    case 'reviews':
      return 'Reviews';
    case 'checks':
      return 'Checks';
    case 'releases':
      return 'Releases';
    default: {
      const unhandled: never = lane;
      return unhandled;
    }
  }
}

function laneSource(lane: JourneyLaneId): SourceClass {
  switch (lane) {
    case 'objective':
      return 'work';
    case 'sessions':
      return 'session';
    case 'agents':
      return 'agent';
    case 'commits':
      return 'commit';
    case 'pull_request':
      return 'pull_request';
    case 'reviews':
      return 'review';
    case 'checks':
      return 'check_result';
    case 'releases':
      return 'release';
    default: {
      const unhandled: never = lane;
      return unhandled;
    }
  }
}

/** The served value of a projection, or `null` when the variant carries none. */
export function projectionValue<T>(projection: {
  readonly state: string;
  readonly value?: T | null;
}): T | null {
  return projection.value ?? null;
}

function reviewGrade(observation: DeliveryReviewObservationV1): EvidenceGrade {
  return observation.provider_outcome === 'stale' ? 'stale' : 'exact';
}

function shortSha(sha: string): string {
  return sha.slice(0, 12);
}

function quarantineReasonLabel(reason: DeliveryGitHubQuarantineReasonV1): string {
  switch (reason) {
    case 'privacy_sanitizer':
      return 'privacy sanitizer';
    case 'body_out_of_bounds':
      return 'body out of bounds';
    default: {
      const unhandled: never = reason;
      return unhandled;
    }
  }
}

function quarantinedDetail(quarantined: readonly DeliveryGitHubQuarantinedCommentV1[]): string[] {
  if (quarantined.length === 0) return [];
  const reasons = [...new Set(quarantined.map((item) => quarantineReasonLabel(item.reason)))];
  return [`${quarantined.length} quarantined (${reasons.join(', ')})`];
}

export interface JourneySelection {
  readonly row: DeliveryInboxPullRequestV1;
  readonly edges: readonly DeliveryMembershipEdgeV1[];
}

export function buildJourney(
  overview: DeliveryOverviewV1,
  selection: JourneySelection,
): JourneyModel {
  const gaps: string[] = [];
  const headBranch = selection.row.branch_ref;
  const headCommit = selection.row.indexed_head_commit_id;

  const membershipEpisodes = (
    lane: JourneyLaneId,
    accept: (edge: DeliveryMembershipEdgeV1) => EpisodeRef | null,
  ): JourneyEpisode[] =>
    selection.edges.flatMap((edge) => {
      const ref = accept(edge);
      if (ref === null) return [];
      return [
        {
          id: `${lane}:${edge.id}`,
          lane,
          label: membershipLabel(ref),
          detail: 'undated · joined by the inbox authority',
          source: membershipSourceClass(edge.basis),
          grade: membershipGrade(edge.basis),
          at: null,
          timeKind: 'undated',
          status: null,
          href: membershipHref(edge.basis),
          ref,
        } satisfies JourneyEpisode,
      ];
    });

  const objective = membershipEpisodes('objective', (edge) =>
    edge.basis.kind === 'shared_work_objective'
      ? { kind: 'work_objective', workItemId: edge.basis.work_item_id }
      : null,
  );
  const sessions = membershipEpisodes('sessions', (edge) =>
    edge.basis.kind === 'session_git_relation'
      ? { kind: 'session', sessionId: edge.basis.session_id, commitId: edge.basis.commit_id }
      : null,
  );
  const agents = membershipEpisodes('agents', (edge) => {
    if (edge.basis.kind === 'shared_agent') return { kind: 'agent', agentId: edge.basis.agent_id };
    if (edge.basis.kind === 'explicit_handoff') {
      return { kind: 'handoff', handoffId: edge.basis.handoff_id };
    }
    return null;
  });

  const usageValue = projectionValue(overview.agent_usage);
  const usageOnBranch = usageValue !== null && shortBranch(headBranch) === usageValue.branch;
  const agentUsage: JourneyEpisode[] =
    usageValue === null || !usageOnBranch
      ? []
      : usageValue.agents.map((usage) => ({
          id: `agents:usage:${usage.provider}:${usage.agent ?? ''}`,
          lane: 'agents',
          label: usage.agent ?? `Unattributed ${usage.provider} sessions`,
          detail: agentUsageDetail(usage),
          source: 'agent',
          // Sessions are placed on the branch by recorded Git spans, not by a
          // join to this pull request's own identity.
          grade: 'inferred',
          at: null,
          timeKind: 'undated',
          status: null,
          href: null,
          ref: { kind: 'agent_usage', branch: usageValue.branch, usage },
        }));

  const commitsValue = projectionValue(overview.commits);
  const commits: JourneyEpisode[] = (commitsValue?.items ?? []).map((commit) => ({
    id: `commits:${commit.commit}`,
    lane: 'commits',
    label: commit.subject,
    detail: `${shortSha(commit.commit)} · ${commit.author_name}`,
    source: 'commit',
    grade: overview.commits.state === 'stale' ? 'stale' : 'exact',
    at: commit.committer_at_micros,
    timeKind: 'event',
    status: null,
    href: compareHref({ branch: headBranch, revision: headCommit, file: null }),
    ref: { kind: 'commit', commit },
  }));

  const pullRequestsValue = projectionValue(overview.pull_requests);
  const pullRequest: JourneyEpisode[] = [];
  if (pullRequestsValue !== null) {
    const item = pullRequestsValue.items.find(
      (candidate) =>
        candidate.pull_request_id === selection.row.pull_request.pull_request_id &&
        candidate.provider === selection.row.pull_request.provider,
    );
    if (item === undefined) {
      gaps.push(
        `Pull request #${selection.row.pull_request.pull_request_id} is not among the ${pullRequestsValue.items.length} head-bound provider items (retained ${pullRequestsValue.total_retained}${pullRequestsValue.truncated ? ', truncated' : ''}).`,
      );
    } else {
      const identityObservation = item.operations.find(
        (operation) => operation.operation === 'pull_request',
      );
      const observedAt =
        identityObservation?.last_complete?.fetched_at_micros ??
        identityObservation?.latest_attempt?.fetched_at_micros ??
        null;
      pullRequest.push({
        id: `pull_request:${item.id}`,
        lane: 'pull_request',
        label: item.identity?.title ?? item.label,
        detail:
          item.identity === null
            ? `#${item.pull_request_id} · identity not served`
            : `#${item.pull_request_id} · ${item.identity.draft ? 'draft · ' : ''}${item.identity.state} · +${item.identity.additions} −${item.identity.deletions} · ${item.identity.changed_files} files`,
        source: 'pull_request',
        grade: item.identity === null ? 'unavailable' : 'exact',
        at: observedAt,
        timeKind: observedAt === null ? 'undated' : 'observed',
        status: null,
        href: compareHref({ branch: headBranch, revision: headCommit, file: null }),
        ref: { kind: 'pull_request', pullRequest: item },
      });
      for (const operation of item.operations) {
        const snapshot = operation.last_complete ?? operation.latest_attempt;
        if (snapshot === null) continue;
        pullRequest.push({
          id: `pull_request:${item.id}:${operation.operation}`,
          lane: 'pull_request',
          label: `${operation.operation.replaceAll('_', ' ')} read`,
          detail: [
            snapshot.outcome,
            snapshot.coverage,
            ...quarantinedDetail(snapshot.quarantined),
            `head ${shortSha(snapshot.provider_head_commit_id)}`,
          ].join(' · '),
          source: 'provider_observation',
          grade: snapshot.outcome === 'stale' ? 'stale' : snapshot.outcome === 'complete' ? 'exact' : 'unavailable',
          at: snapshot.fetched_at_micros,
          timeKind: 'observed',
          status: null,
          href: null,
          ref: {
            kind: 'provider_observation',
            operation: operation.operation,
            outcome: snapshot.outcome,
            fetchedAtMicros: snapshot.fetched_at_micros,
          },
        });
      }
    }
  }

  const reviewsValue = projectionValue(overview.review_comments);
  const reviews: JourneyEpisode[] = (reviewsValue?.items ?? []).flatMap((item) => {
    const observation = latestObservation(item);
    if (observation === null) return [];
    return [
      {
        id: `reviews:${item.id}`,
        lane: 'reviews',
        label: observation.line === null ? observation.path : `${observation.path}:${observation.line}`,
        detail: `${observation.review_state.replaceAll('_', ' ')} · ${observation.lifecycle} · ${observation.author_class.replaceAll('_', ' ')}`,
        source: 'review',
        grade: reviewGrade(observation),
        at: observation.observed_at_micros,
        timeKind: 'observed',
        status: null,
        href: compareHref({ branch: headBranch, revision: headCommit, file: observation.path }),
        ref: { kind: 'review', item, observation },
      } satisfies JourneyEpisode,
    ];
  });

  const checksValue = projectionValue(overview.ci_checks);
  const checks: JourneyEpisode[] = (checksValue?.items ?? []).map((check) => ({
    id: `checks:${check.id}`,
    lane: 'checks',
    label: check.label,
    detail: `${check.workflow_path} · ${check.check_status}${check.check_conclusion === null ? '' : ` · ${check.check_conclusion}`}`,
    source: 'check_result',
    grade: overview.ci_checks.state === 'stale' ? 'stale' : 'exact',
    at: check.observed_at_micros,
    timeKind: 'observed',
    status: checkStatusKind(check),
    href: compareHref({
      branch: headBranch,
      revision: headCommit,
      file: check.annotations[0]?.path ?? null,
    }),
    ref: { kind: 'check', check },
  }));

  const releasesValue = projectionValue(overview.releases);
  const releases: JourneyEpisode[] = (releasesValue?.items ?? []).map((release) => ({
    id: `releases:${release.id}`,
    lane: 'releases',
    label: release.tag,
    detail: `${release.draft ? 'draft' : release.prerelease ? 'prerelease' : 'release'} · ${release.assets.length} assets`,
    source: 'release',
    grade: overview.releases.state === 'stale' ? 'stale' : 'exact',
    at: release.published_at_micros ?? release.created_at_micros,
    timeKind: 'event',
    status: null,
    href: null,
    ref: { kind: 'release', release },
  }));

  const membershipLane = (
    id: JourneyLaneId,
    episodes: JourneyEpisode[],
    absent: string,
  ): JourneyLane => ({
    id,
    label: laneLabel(id),
    source: laneSource(id),
    state: episodes.length === 0 ? { kind: 'unavailable', detail: absent } : { kind: 'served' },
    episodes,
  });

  const projectionLane = (
    id: JourneyLaneId,
    projection: AnyProjection,
    episodes: JourneyEpisode[],
  ): JourneyLane => ({
    id,
    label: laneLabel(id),
    source: laneSource(id),
    state: projectionLaneState(projection, laneLabel(id)),
    episodes,
  });

  const lanes: JourneyLane[] = [
    membershipLane(
      'objective',
      objective,
      'No Work objective is joined to this pull request by the inbox authority.',
    ),
    membershipLane(
      'sessions',
      sessions,
      'No session–Git relation is joined to this pull request; transcript provenance is not inferred.',
    ),
    {
      id: 'agents',
      label: laneLabel('agents'),
      source: laneSource('agents'),
      state: agentLaneState(overview.agent_usage, usageValue, headBranch, agents.length > 0),
      episodes: [...agents, ...agentUsage],
    },
    projectionLane('commits', overview.commits, commits),
    projectionLane('pull_request', overview.pull_requests, pullRequest),
    projectionLane('reviews', overview.review_comments, reviews),
    projectionLane('checks', overview.ci_checks, checks),
    projectionLane('releases', overview.releases, releases),
  ];

  const episodes = lanes.flatMap((lane) => lane.episodes);
  const dated = episodes.filter((episode) => episode.at !== null).map((episode) => episode.at as number);
  const span =
    dated.length === 0
      ? null
      : { start: Math.min(...dated), end: Math.max(...dated) };

  return {
    lanes,
    episodes,
    span,
    undated: episodes.length - dated.length,
    gaps,
  };
}

function shortBranch(ref: string): string {
  return ref.replace(/^refs\/heads\//, '');
}

/** Tokens as the provider reported them: the total when every session sent
 * one, else input plus output, and a lower bound whenever a session is
 * missing usage. Nothing is estimated from transcript text. */
export function agentTokenLabel(usage: DeliveryAgentUsageRowV1): string {
  if (usage.sessions_with_usage === 0) return 'tokens not reported';
  const { total_tokens, input_tokens, output_tokens } = usage.counters;
  const tokens =
    total_tokens ?? (input_tokens !== null && output_tokens !== null ? input_tokens + output_tokens : null);
  if (tokens === null) return 'token counters incomplete';
  const figure = `${tokens.toLocaleString('en-US')} tokens`;
  if (usage.usage_complete) return figure;
  return `≥${figure} (usage for ${usage.sessions_with_usage} of ${usage.sessions} sessions)`;
}

function agentUsageDetail(usage: DeliveryAgentUsageRowV1): string {
  const sessions = `${usage.sessions} ${usage.sessions === 1 ? 'session' : 'sessions'}`;
  return `${usage.provider} · ${sessions} · ${agentTokenLabel(usage)} · ${usage.tool_calls.toLocaleString('en-US')} tool calls`;
}

function agentLaneState(
  projection: DeliveryOverviewV1['agent_usage'],
  usage: DeliveryAgentUsageV1 | null,
  headBranch: string,
  hasMembership: boolean,
): LaneState {
  if (usage !== null && usage.branch !== shortBranch(headBranch)) {
    return {
      kind: 'unavailable',
      detail: `Agent usage was read for the checkout's branch ${usage.branch}, not this pull request's head ${shortBranch(headBranch)}.`,
      requiredAuthority: 'session-Git correlation index',
    };
  }
  if (usage !== null && projection.state === 'partial') {
    const reasons = [
      usage.truncated ? 'the correlation read reached its session ceiling' : null,
      usage.usage_coverage === 'complete'
        ? null
        : `provider usage coverage is ${usage.usage_coverage}, so token counts are lower bounds`,
    ].filter((reason): reason is string => reason !== null);
    return { kind: 'partial', detail: `Agent usage: ${reasons.join('; ')}` };
  }
  const projected = projectionLaneState(projection, 'Agent usage');
  if (hasMembership && !laneServes(projected)) {
    return { kind: 'served', detail: `Agent usage: ${projected.detail}` };
  }
  return projected;
}

function membershipLabel(ref: EpisodeRef): string {
  switch (ref.kind) {
    case 'work_objective':
      return `Work objective ${ref.workItemId}`;
    case 'session':
      return `Session ${ref.sessionId}`;
    case 'agent':
      return `Agent ${ref.agentId}`;
    case 'handoff':
      return `Handoff ${ref.handoffId}`;
    case 'agent_usage':
    case 'commit':
    case 'pull_request':
    case 'provider_observation':
    case 'review':
    case 'check':
    case 'release':
      return '';
    default: {
      const unhandled: never = ref;
      return unhandled;
    }
  }
}
