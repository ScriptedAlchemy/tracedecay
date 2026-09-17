import type {
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
  DeliveryMembershipBasisV1,
  DeliveryMembershipEdgeV1,
} from '../../contracts/generated.ts';
import {
  isCorrelatingBasis,
  membershipBasisLabel,
  membershipGrade,
  membershipIdentity,
  membershipSourceClass,
  type EvidenceGrade,
  type SourceClass,
} from './evidence.ts';

/**
 * An umbrella Delivery is a correlation projection, not a provider object:
 * pull requests that share one served basis identity — a Work objective, a
 * handoff token, a session, an agent — grouped under that identity. Each
 * basis kind forms its own umbrellas; two bases are never merged into one
 * outcome, because "same agent" and "same Work objective" are different
 * claims with different grades and a reviewer must be able to tell which one
 * grouped the PRs in front of them.
 */
export interface UmbrellaEdge {
  readonly id: string;
  readonly pullRequestId: string;
  readonly projectId: string;
  readonly basis: DeliveryMembershipBasisV1;
  readonly grade: EvidenceGrade;
  readonly source: SourceClass;
}

export interface UmbrellaMember {
  /** Inbox row id (`project:provider:number`). */
  readonly id: string;
  readonly projectId: string;
  readonly pullRequest: DeliveryInboxPullRequestV1;
  readonly edge: UmbrellaEdge;
}

export interface Umbrella {
  readonly id: string;
  readonly basisKind: DeliveryMembershipBasisV1['kind'];
  readonly basisLabel: string;
  readonly identity: string;
  readonly grade: EvidenceGrade;
  readonly source: SourceClass;
  readonly members: readonly UmbrellaMember[];
  readonly projectIds: readonly string[];
  readonly activeAttention: number;
}

export type CorrelationAuthority =
  | { readonly state: 'served'; readonly edges: number }
  | { readonly state: 'unavailable'; readonly reason: string };

export interface UmbrellaProjection {
  readonly authority: CorrelationAuthority;
  readonly umbrellas: readonly Umbrella[];
  /** Correlating edges whose pull request is not in the admitted inbox: kept
   * visible as a count so a dropped join never disappears silently. */
  readonly unresolvedEdges: number;
}

/** Order umbrellas by the strength of their basis, then by identity. */
const BASIS_RANK: Record<DeliveryMembershipBasisV1['kind'], number> = {
  shared_work_objective: 0,
  explicit_handoff: 1,
  session_git_relation: 2,
  shared_agent: 3,
  branch_pull_request_reference: 4,
};

function edgeKey(edge: DeliveryMembershipEdgeV1): string {
  return `${edge.project_id}:${edge.pull_request_id}`;
}

function rowKey(row: DeliveryInboxPullRequestV1): string {
  return `${row.project_id}:${row.pull_request.pull_request_id}`;
}

export function toUmbrellaEdge(edge: DeliveryMembershipEdgeV1): UmbrellaEdge {
  return {
    id: edge.id,
    pullRequestId: edge.pull_request_id,
    projectId: edge.project_id,
    basis: edge.basis,
    grade: membershipGrade(edge.basis),
    source: membershipSourceClass(edge.basis),
  };
}

export function buildUmbrellas(inbox: DeliveryInboxV1): UmbrellaProjection {
  const rows = new Map<string, DeliveryInboxPullRequestV1>();
  for (const row of inbox.pull_requests) rows.set(rowKey(row), row);

  const correlating = inbox.membership_edges.filter((edge) => isCorrelatingBasis(edge.basis));
  if (correlating.length === 0) {
    return {
      authority: {
        state: 'unavailable',
        reason:
          'The inbox authority served no cross-PR correlation edge (shared Work objective, session–Git relation, handoff, or shared agent). Umbrella grouping is not inferred from proximity.',
      },
      umbrellas: [],
      unresolvedEdges: 0,
    };
  }

  const groups = new Map<string, { basis: DeliveryMembershipBasisV1; members: UmbrellaMember[] }>();
  let unresolvedEdges = 0;
  for (const edge of correlating) {
    const row = rows.get(edgeKey(edge));
    if (row === undefined) {
      unresolvedEdges += 1;
      continue;
    }
    const identity = membershipIdentity(edge.basis);
    const groupId = `${edge.basis.kind}:${identity}`;
    const group = groups.get(groupId) ?? { basis: edge.basis, members: [] };
    if (!group.members.some((member) => member.id === row.id)) {
      group.members.push({
        id: row.id,
        projectId: row.project_id,
        pullRequest: row,
        edge: toUmbrellaEdge(edge),
      });
    }
    groups.set(groupId, group);
  }

  const umbrellas = [...groups.entries()]
    .filter(([, group]) => group.members.length >= 2)
    .map(([id, group]) => {
      const members = [...group.members].sort((left, right) => left.id.localeCompare(right.id));
      return {
        id,
        basisKind: group.basis.kind,
        basisLabel: membershipBasisLabel(group.basis),
        identity: membershipIdentity(group.basis),
        grade: membershipGrade(group.basis),
        source: membershipSourceClass(group.basis),
        members,
        projectIds: [...new Set(members.map((member) => member.projectId))].sort(),
        activeAttention: members.reduce(
          (total, member) =>
            total +
            member.pullRequest.attention.filter((item) => item.state === 'active').length,
          0,
        ),
      } satisfies Umbrella;
    })
    .sort(
      (left, right) =>
        BASIS_RANK[left.basisKind] - BASIS_RANK[right.basisKind] ||
        left.identity.localeCompare(right.identity),
    );

  return {
    authority: { state: 'served', edges: correlating.length },
    umbrellas,
    unresolvedEdges,
  };
}

/** Umbrellas a given inbox row belongs to. */
export function umbrellasFor(
  projection: UmbrellaProjection,
  pullRequestRowId: string,
): readonly Umbrella[] {
  return projection.umbrellas.filter((umbrella) =>
    umbrella.members.some((member) => member.id === pullRequestRowId),
  );
}

/**
 * Cross-project rows correlated with the scoped project: every member of an
 * umbrella that also contains a scoped-project PR, minus the scoped rows
 * themselves. Proximity alone never places a row here.
 */
export function relatedAcrossProjects(
  projection: UmbrellaProjection,
  projectId: string,
): readonly { member: UmbrellaMember; umbrella: Umbrella }[] {
  const seen = new Set<string>();
  const related: { member: UmbrellaMember; umbrella: Umbrella }[] = [];
  for (const umbrella of projection.umbrellas) {
    if (!umbrella.projectIds.includes(projectId)) continue;
    for (const member of umbrella.members) {
      if (member.projectId === projectId || seen.has(member.id)) continue;
      seen.add(member.id);
      related.push({ member, umbrella });
    }
  }
  return related;
}
