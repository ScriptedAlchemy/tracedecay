import type {
  DeliveryAttentionSourceV1,
  DeliveryInboxPullRequestV1,
  DeliveryMembershipEdgeV1,
} from '../../contracts/generated.ts';
import {
  EVIDENCE_GRADES,
  gradeLabel,
  membershipGrade,
  membershipIdentity,
  membershipSourceClass,
  type EvidenceGrade,
  type SourceClass,
} from './evidence.ts';
import { laneServes, type JourneyEpisode, type JourneyLaneId, type JourneyModel } from './journey.ts';
import { attentionSourceLabel, evidenceIdentity } from './PullRequestInspector.tsx';
import { laneStateDetail } from './ProjectionLedger.tsx';
import { attentionCode, headJoin, isVerificationSource } from './rendererModel.ts';

/**
 * Renderer B, the journey transit: one pull request read as a left-to-right
 * chain of four stations, agent session → code change → CI / review → next
 * action. A station the joined authorities did not serve is kept as a NO
 * EVIDENCE band carrying the daemon's reason; it is never skipped, and no
 * agent reasoning is reconstructed to fill it. "Next action" prints only the
 * attention the inbox bound to named sources; nothing is recommended here.
 */
export const TRANSIT_STATIONS = ['session', 'code', 'verification', 'next'] as const;
export type TransitStationId = (typeof TRANSIT_STATIONS)[number];

export type StationState = 'evidence' | 'served_empty' | 'no_evidence';

export interface TransitItem {
  readonly id: string;
  readonly label: string;
  readonly detail: string;
  readonly grade: EvidenceGrade;
  readonly source: SourceClass;
  readonly at: number | null;
  readonly timeKind: 'event' | 'observed' | 'undated';
  /** The journey episode this item selects, when a journey model is loaded. */
  readonly episodeId: string | null;
  readonly attention: DeliveryAttentionSourceV1 | null;
}

export interface TransitBranch {
  readonly kind: 'objective' | 'session' | 'agent' | 'handoff';
  readonly identities: readonly string[];
}

export interface TransitStation {
  readonly id: TransitStationId;
  readonly title: string;
  readonly state: StationState;
  /** The weakest grade supporting the station; `unavailable` when empty. */
  readonly grade: EvidenceGrade;
  readonly gradeCounts: readonly { readonly grade: EvidenceGrade; readonly count: number }[];
  readonly reasons: readonly string[];
  readonly items: readonly TransitItem[];
  readonly span: { readonly start: number; readonly end: number } | null;
  readonly branches: readonly TransitBranch[];
}

export interface TransitLink {
  readonly from: TransitStationId;
  readonly to: TransitStationId;
  readonly grade: EvidenceGrade;
  readonly basis: string;
}

export interface TransitModel {
  readonly stations: readonly TransitStation[];
  readonly links: readonly TransitLink[];
  readonly span: { readonly start: number; readonly end: number } | null;
}

function membershipLane(kind: DeliveryMembershipEdgeV1['basis']['kind']): JourneyLaneId | null {
  switch (kind) {
    case 'shared_work_objective':
      return 'objective';
    case 'session_git_relation':
      return 'sessions';
    case 'shared_agent':
    case 'explicit_handoff':
      return 'agents';
    case 'branch_pull_request_reference':
      return null;
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

function branchKind(kind: DeliveryMembershipEdgeV1['basis']['kind']): TransitBranch['kind'] | null {
  switch (kind) {
    case 'shared_work_objective':
      return 'objective';
    case 'session_git_relation':
      return 'session';
    case 'shared_agent':
      return 'agent';
    case 'explicit_handoff':
      return 'handoff';
    case 'branch_pull_request_reference':
      return null;
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

function spanOf(items: readonly TransitItem[]): TransitStation['span'] {
  const dated = items.map((item) => item.at).filter((at): at is number => at !== null);
  return dated.length === 0 ? null : { start: Math.min(...dated), end: Math.max(...dated) };
}

function station(
  id: TransitStationId,
  title: string,
  items: readonly TransitItem[],
  reasons: readonly string[],
  emptyState: Exclude<StationState, 'evidence'>,
  branches: readonly TransitBranch[] = [],
): TransitStation {
  const counts = EVIDENCE_GRADES.map((grade) => ({ grade, count: items.filter((item) => item.grade === grade).length })).filter(
    (entry) => entry.count > 0,
  );
  const weakest = counts[counts.length - 1]?.grade ?? 'unavailable';
  return {
    id,
    title,
    state: items.length > 0 ? 'evidence' : emptyState,
    grade: items.length > 0 ? weakest : 'unavailable',
    gradeCounts: counts,
    reasons,
    items,
    span: spanOf(items),
    branches,
  };
}

function shortSha(sha: string): string {
  return sha.slice(0, 7);
}

export function buildTransit(
  row: DeliveryInboxPullRequestV1,
  edges: readonly DeliveryMembershipEdgeV1[],
  journey: JourneyModel | null,
): TransitModel {
  const episodeIds = new Set(journey?.episodes.map((episode) => episode.id) ?? []);
  const episodeId = (id: string): string | null => (episodeIds.has(id) ? id : null);
  const lane = (id: JourneyLaneId) => journey?.lanes.find((candidate) => candidate.id === id) ?? null;
  const laneReason = (id: JourneyLaneId, label: string): string[] => {
    const found = lane(id);
    if (found === null || laneServes(found.state)) return [];
    return [`${label} · ${found.state.kind.replaceAll('_', ' ')} · ${laneStateDetail(found.state) ?? found.state.detail}`];
  };
  const episodeItem = (episode: JourneyEpisode): TransitItem => ({
      id: episode.id,
      label: episode.label,
      detail: episode.detail,
      grade: episode.grade,
      source: episode.source,
      at: episode.at,
      timeKind: episode.timeKind,
      episodeId: episode.id,
      attention: null,
    });
  const laneItems = (id: JourneyLaneId): TransitItem[] => (lane(id)?.episodes ?? []).map(episodeItem);

  // Agent session: only served membership bases; reasoning is never drawn.
  const joined = edges.filter((edge) => membershipLane(edge.basis.kind) !== null);
  const sessionItems: TransitItem[] = joined.map((edge) => ({
    id: `session:${edge.id}`,
    label: `${edge.basis.kind.replaceAll('_', ' ')} ${membershipIdentity(edge.basis)}`,
    detail: 'undated · joined by the inbox authority',
    grade: membershipGrade(edge.basis),
    source: membershipSourceClass(edge.basis),
    at: null,
    timeKind: 'undated',
    episodeId: episodeId(`${membershipLane(edge.basis.kind)}:${edge.id}`),
    attention: null,
  }));
  const usageEpisodes = (lane('agents')?.episodes ?? []).filter((episode) => episode.ref.kind === 'agent_usage');
  sessionItems.push(...usageEpisodes.map(episodeItem));
  const branches: TransitBranch[] = (['objective', 'session', 'agent', 'handoff'] as const).map((kind) => ({
    kind,
    identities: [
      ...new Set([
        ...joined.filter((edge) => branchKind(edge.basis.kind) === kind).map((edge) => membershipIdentity(edge.basis)),
        ...(kind === 'agent' ? usageEpisodes.map((episode) => episode.label) : []),
      ]),
    ].sort(),
  }));
  const session = station(
    'session',
    'Agent session',
    sessionItems,
    [
      ...(sessionItems.length === 0
        ? ['No session–Git relation, agent attribution, handoff or Work objective is joined to this pull request.', 'Agent reasoning is not reconstructed.']
        : ['Persisted joins only; private agent reasoning is unavailable.']),
      ...laneReason('agents', 'Agent usage'),
    ],
    'no_evidence',
    branches,
  );

  // Code change: the served identity, the indexed head, then local commits.
  const identity = row.pull_request.identity;
  const codeItems: TransitItem[] = [
    {
      id: `code:head:${row.id}`,
      label: `${row.branch_ref.replace(/^refs\/heads\//, '')} @ ${shortSha(row.indexed_head_commit_id)}`,
      detail: `indexed head · ${row.indexed_generation}`,
      grade: 'exact',
      source: 'index',
      at: null,
      timeKind: 'undated',
      episodeId: null,
      attention: null,
    },
    ...(identity === null
      ? []
      : [
          {
            id: `code:identity:${row.id}`,
            label: `+${identity.additions.toLocaleString('en-US')} −${identity.deletions.toLocaleString('en-US')} · ${identity.changed_files.toLocaleString('en-US')} files`,
            detail: `${identity.draft ? 'draft · ' : ''}${identity.state} · provider identity`,
            grade: 'exact' as const,
            source: 'pull_request' as const,
            at: null,
            timeKind: 'undated' as const,
            episodeId: episodeId(`pull_request:${row.pull_request.id}`),
            attention: null,
          },
        ]),
    ...laneItems('commits'),
  ];
  const code = station(
    'code',
    'Code change',
    codeItems,
    [...(identity === null ? ['Provider identity not served · line change unknown.'] : []), ...laneReason('commits', 'Commits')],
    'no_evidence',
  );

  // CI / review: provider review reads, verification attention, then the
  // project's served checks and review threads.
  const join = headJoin(row);
  const readItems: TransitItem[] = row.pull_request.operations.flatMap((operation) => {
    if (operation.operation === 'pull_request') return [];
    const snapshot = operation.last_complete ?? operation.latest_attempt;
    if (snapshot === null) return [];
    return [
      {
        id: `verification:read:${operation.operation}`,
        label: `${operation.operation.replaceAll('_', ' ')} read · ${snapshot.outcome}`,
        detail: `${snapshot.coverage} coverage · head ${shortSha(snapshot.provider_head_commit_id)}`,
        grade: snapshot.outcome === 'complete' ? ('exact' as const) : snapshot.outcome === 'stale' ? ('stale' as const) : ('unavailable' as const),
        source: 'provider_observation' as const,
        at: snapshot.fetched_at_micros,
        timeKind: 'observed' as const,
        episodeId: episodeId(`pull_request:${row.pull_request.id}:${operation.operation}`),
        attention: null,
      },
    ];
  });
  const attentionItem = (item: DeliveryInboxPullRequestV1['attention'][number], prefix: string): TransitItem => ({
    id: `${prefix}:${item.id}`,
    label: `${attentionCode(item.source)} · ${attentionSourceLabel(item.source)}`,
    detail: item.evidence.length === 0 ? `${item.coverage} coverage · no evidence record` : item.evidence.map(evidenceIdentity).join(' · '),
    grade: 'exact',
    source: item.source === 'ci_failure' || item.source === 'test_risk' ? 'check_result' : 'review',
    at: item.observed_at_micros,
    timeKind: item.observed_at_micros === null ? 'undated' : 'observed',
    episodeId: null,
    attention: item.source,
  });
  const verificationAttention = row.attention
    .filter((item) => item.state === 'active' && isVerificationSource(item.source))
    .map((item) => attentionItem(item, 'verification'));
  const verificationReasons = [...laneReason('checks', 'Checks'), ...laneReason('reviews', 'Reviews')];
  const verificationItems = [...verificationAttention, ...laneItems('checks'), ...laneItems('reviews'), ...readItems];
  const verification = station(
    'verification',
    'CI / review',
    verificationItems,
    verificationItems.length === 0 && verificationReasons.length === 0
      ? ['No check, review thread or review read was served for this head.']
      : verificationReasons,
    verificationReasons.length === 0 && journey !== null ? 'served_empty' : 'no_evidence',
  );

  // Next action: the open attention the inbox bound to named sources.
  const active = row.attention.filter((item) => item.state === 'active');
  const unevaluated = row.attention.filter((item) => item.state === 'unavailable' || item.state === 'denied');
  const next = station(
    'next',
    'Next action',
    active.map((item) => attentionItem(item, 'next')),
    [
      ...unevaluated.map((item) => `${attentionCode(item.source)} not evaluated · ${item.state} · ${item.coverage} coverage`),
      ...(active.length === 0 && unevaluated.length === 0 ? ['No active attention served · nothing is recommended.'] : []),
    ],
    unevaluated.length > 0 && active.length === 0 ? 'no_evidence' : 'served_empty',
  );

  const stations = [session, code, verification, next];
  const present = (entry: TransitStation) => entry.state !== 'no_evidence';
  const sessionBasis = (): { grade: EvidenceGrade; basis: string } => {
    const kinds = new Set(joined.map((edge) => edge.basis.kind));
    if (kinds.has('session_git_relation')) return { grade: 'inferred', basis: 'session–Git relation' };
    if (kinds.has('explicit_handoff')) return { grade: 'explicit', basis: 'handoff token' };
    if (kinds.has('shared_agent')) return { grade: 'inferred', basis: 'shared agent' };
    return { grade: 'explicit', basis: 'Work objective' };
  };
  const headLink = (): { grade: EvidenceGrade; basis: string } => {
    switch (join.kind) {
      case 'joined':
        return { grade: 'exact', basis: 'provider head = indexed head' };
      case 'provider_head_moved':
        return { grade: 'stale', basis: 'provider head moved' };
      case 'not_observed':
        return { grade: 'unavailable', basis: 'provider head not observed' };
      default: {
        const unhandled: never = join;
        return unhandled;
      }
    }
  };
  const unjoined = { grade: 'unavailable' as const, basis: 'no evidence to join' };
  const links: TransitLink[] = [
    { from: 'session', to: 'code', ...(present(session) && present(code) ? sessionBasis() : unjoined) },
    { from: 'code', to: 'verification', ...(verification.state === 'evidence' ? headLink() : unjoined) },
    {
      from: 'verification',
      to: 'next',
      ...(present(next) && present(verification) ? { grade: 'exact' as const, basis: 'attention bound to named sources' } : unjoined),
    },
  ];
  const spans = stations.map((entry) => entry.span).filter((span): span is NonNullable<typeof span> => span !== null);
  return {
    stations,
    links,
    span: spans.length === 0 ? null : { start: Math.min(...spans.map((span) => span.start)), end: Math.max(...spans.map((span) => span.end)) },
  };
}

export function stationStateLabel(state: StationState): string {
  switch (state) {
    case 'evidence':
      return 'EVIDENCE';
    case 'served_empty':
      return 'SERVED EMPTY';
    case 'no_evidence':
      return 'NO EVIDENCE';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

export function gradeSummary(entry: TransitStation): string {
  return entry.gradeCounts.length === 0
    ? gradeLabel('unavailable')
    : entry.gradeCounts.map(({ grade, count }) => `${count} ${gradeLabel(grade)}`).join(' · ');
}
