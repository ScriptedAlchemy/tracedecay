import type {
  DeliveryAttentionItemV1,
  DeliveryAttentionSourceV1,
  DeliveryInboxPullRequestV1,
  DeliveryMembershipBasisV1,
} from '../../contracts/generated.ts';
import type { EvidenceGrade } from './evidence.ts';
import type { UmbrellaProjection } from './umbrella.ts';

/**
 * Readings the candidate Delivery renderers share. Each is a fixed
 * presentation of a served field; none infers a relation, a size, a time or
 * an attention state the inbox did not serve.
 */

/** The short engraved code an amber attention beacon prints beside its glyph. */
export function attentionCode(source: DeliveryAttentionSourceV1): string {
  switch (source) {
    case 'ci_failure':
      return 'CI';
    case 'confirmed_conflict':
      return 'CONFLICT';
    case 'contradiction':
      return 'CONTRA';
    case 'divergent_shared_implementation':
      return 'DIVERGE';
    case 'evidence_gap':
      return 'GAP';
    case 'new_review_comment':
      return 'NEW REV';
    case 'overlapping_edit':
      return 'OVERLAP';
    case 'stale_provider_state':
      return 'STALE';
    case 'test_risk':
      return 'TEST';
    case 'unresolved_review':
      return 'REVIEW';
    case 'unreviewed_changed_code':
      return 'UNREV';
    case 'unsafe_pattern':
      return 'UNSAFE';
    case 'weak_evidence':
      return 'WEAK';
    default: {
      const unhandled: never = source;
      return unhandled;
    }
  }
}

/** Whether an attention source is CI or review evidence, the transit's
 * verification station, rather than a cross-work or freshness signal. */
export function isVerificationSource(source: DeliveryAttentionSourceV1): boolean {
  switch (source) {
    case 'ci_failure':
    case 'test_risk':
    case 'unresolved_review':
    case 'new_review_comment':
    case 'unreviewed_changed_code':
      return true;
    case 'confirmed_conflict':
    case 'contradiction':
    case 'divergent_shared_implementation':
    case 'evidence_gap':
    case 'overlapping_edit':
    case 'stale_provider_state':
    case 'unsafe_pattern':
    case 'weak_evidence':
      return false;
    default: {
      const unhandled: never = source;
      return unhandled;
    }
  }
}

export function activeItems(row: DeliveryInboxPullRequestV1): readonly DeliveryAttentionItemV1[] {
  return row.attention.filter((item) => item.state === 'active');
}

/** Attention the daemon could not evaluate: printed as a typed gap, never amber. */
export function unevaluatedItems(row: DeliveryInboxPullRequestV1): readonly DeliveryAttentionItemV1[] {
  return row.attention.filter((item) => item.state === 'unavailable' || item.state === 'denied');
}

/** Lines changed as served by the provider identity, or `null` when the
 * identity was not served. A missing size is never drawn as zero. */
export function measuredChange(row: DeliveryInboxPullRequestV1): number | null {
  const identity = row.pull_request.identity;
  return identity === null ? null : identity.additions + identity.deletions;
}

/** Mark radius on a log scale of measured change, so a 100k-line PR and a
 * 10-line PR are both legible. `null` change has no radius of its own. */
export function changeRadius(change: number, ceiling: number, min = 4, max = 15): number {
  const top = Math.log10(1 + Math.max(ceiling, 1));
  return min + (max - min) * (Math.log10(1 + Math.max(change, 0)) / top);
}

export type HeadJoin =
  | { readonly kind: 'joined'; readonly head: string }
  | { readonly kind: 'provider_head_moved'; readonly providerHead: string; readonly indexedHead: string }
  | { readonly kind: 'not_observed' };

/** How the provider's last observed head relates to the indexed head the
 * inbox admitted the row on. */
export function headJoin(row: DeliveryInboxPullRequestV1): HeadJoin {
  const heads = row.pull_request.operations
    .map((operation) => (operation.last_complete ?? operation.latest_attempt)?.provider_head_commit_id)
    .filter((head): head is string => head !== undefined);
  if (heads.length === 0) return { kind: 'not_observed' };
  const moved = heads.find((head) => head !== row.indexed_head_commit_id);
  return moved === undefined
    ? { kind: 'joined', head: row.indexed_head_commit_id }
    : { kind: 'provider_head_moved', providerHead: moved, indexedHead: row.indexed_head_commit_id };
}

export function headJoinSentence(join: HeadJoin): string {
  switch (join.kind) {
    case 'joined':
      return `provider head = indexed head ${join.head.slice(0, 7)}`;
    case 'provider_head_moved':
      return `provider head ${join.providerHead.slice(0, 7)} ≠ indexed head ${join.indexedHead.slice(0, 7)}`;
    case 'not_observed':
      return 'provider head not observed · no read snapshot served';
    default: {
      const unhandled: never = join;
      return unhandled;
    }
  }
}

export interface ObservationMark {
  readonly at: number;
  readonly kind: 'provider_read' | 'attention';
  readonly label: string;
  readonly attention: DeliveryAttentionItemV1 | null;
}

/** Every timestamp the inbox row carries: provider read snapshots and
 * attention observations. All are daemon observation time, not PR event time. */
export function observationMarks(row: DeliveryInboxPullRequestV1): readonly ObservationMark[] {
  const marks: ObservationMark[] = [];
  for (const operation of row.pull_request.operations) {
    const snapshot = operation.last_complete ?? operation.latest_attempt;
    if (snapshot === null) continue;
    marks.push({
      at: snapshot.fetched_at_micros,
      kind: 'provider_read',
      label: `${operation.operation.replaceAll('_', ' ')} read · ${snapshot.outcome}`,
      attention: null,
    });
  }
  for (const item of row.attention) {
    if (item.observed_at_micros === null) continue;
    marks.push({
      at: item.observed_at_micros,
      kind: 'attention',
      label: `${attentionCode(item.source)} · ${item.state}`,
      attention: item,
    });
  }
  return marks.sort((left, right) => left.at - right.at || left.label.localeCompare(right.label));
}

export function observationWindow(
  row: DeliveryInboxPullRequestV1,
): { readonly start: number; readonly end: number } | null {
  const marks = observationMarks(row);
  if (marks.length === 0) return null;
  return { start: marks[0]!.at, end: marks[marks.length - 1]!.at };
}

export function basisKindCode(kind: DeliveryMembershipBasisV1['kind']): string {
  switch (kind) {
    case 'shared_work_objective':
      return 'WORK';
    case 'explicit_handoff':
      return 'HANDOFF';
    case 'session_git_relation':
      return 'SESSION';
    case 'shared_agent':
      return 'AGENT';
    case 'branch_pull_request_reference':
      return 'BRANCH';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** One drawn joined-evidence link between two admitted PRs that share a
 * served basis identity. Members of a group chain in id order, so no hub,
 * objective or umbrella node is ever invented to anchor them. */
export interface EvidenceLink {
  readonly id: string;
  readonly from: string;
  readonly to: string;
  readonly kind: DeliveryMembershipBasisV1['kind'];
  readonly code: string;
  readonly identity: string;
  readonly grade: EvidenceGrade;
  readonly groupSize: number;
}

export function evidenceLinks(
  projection: UmbrellaProjection,
  visible: ReadonlySet<string>,
): readonly EvidenceLink[] {
  const links: EvidenceLink[] = [];
  for (const umbrella of projection.umbrellas) {
    const members = umbrella.members.filter((member) => visible.has(member.id));
    for (let index = 1; index < members.length; index += 1) {
      const from = members[index - 1]!;
      const to = members[index]!;
      links.push({
        id: `${umbrella.id}:${from.id}->${to.id}`,
        from: from.id,
        to: to.id,
        kind: umbrella.basisKind,
        code: basisKindCode(umbrella.basisKind),
        identity: umbrella.identity,
        grade: umbrella.grade,
        groupSize: members.length,
      });
    }
  }
  return links;
}

/** Rows that share no served correlating identity with any other admitted
 * row. They are drawn hollow with this absence printed, never linked by
 * repository membership. */
export function uncorrelatedRows(
  projection: UmbrellaProjection,
  rows: readonly DeliveryInboxPullRequestV1[],
): ReadonlySet<string> {
  const grouped = new Set(projection.umbrellas.flatMap((umbrella) => umbrella.members.map((member) => member.id)));
  return new Set(rows.filter((row) => !grouped.has(row.id)).map((row) => row.id));
}

export const UNCORRELATED_SENTENCE = 'no correlating evidence served';
