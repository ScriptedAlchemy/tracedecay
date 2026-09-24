import type {
  DeliveryGitHubOutcomeV1,
  DeliveryInboxCoverageV1,
  DeliveryMembershipBasisV1,
  DeliveryProviderStateV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import { gradeDashArray } from '../../viz/temporal/palette.ts';

/**
 * The evidence-grade ladder Delivery renders beside every relation, identity,
 * count, and status. Grade is an ordered description of the support behind a
 * displayed claim, never a confidence percentage. It is orthogonal to the
 * source class (`COMMIT`, `PR`, `REVIEW`, `CHECK RESULT`, …), which says where
 * the evidence was persisted or observed.
 */
export const EVIDENCE_GRADES = [
  'exact',
  'explicit',
  'inferred',
  'ambiguous',
  'stale',
  'unavailable',
] as const;

export type EvidenceGrade = (typeof EVIDENCE_GRADES)[number];

export type SourceClass =
  | 'registry'
  | 'commit'
  | 'pull_request'
  | 'review'
  | 'check_result'
  | 'release'
  | 'work'
  | 'session'
  | 'agent'
  | 'handoff'
  | 'provider_observation'
  | 'index';

export function gradeLabel(grade: EvidenceGrade): string {
  switch (grade) {
    case 'exact':
      return 'EXACT';
    case 'explicit':
      return 'EXPLICIT';
    case 'inferred':
      return 'INFERRED';
    case 'ambiguous':
      return 'AMBIGUOUS';
    case 'stale':
      return 'STALE';
    case 'unavailable':
      return 'UNAVAILABLE';
    default: {
      const unhandled: never = grade;
      return unhandled;
    }
  }
}

export function sourceClassLabel(source: SourceClass): string {
  switch (source) {
    case 'registry':
      return 'REGISTRY';
    case 'commit':
      return 'COMMIT';
    case 'pull_request':
      return 'PR';
    case 'review':
      return 'REVIEW';
    case 'check_result':
      return 'CHECK RESULT';
    case 'release':
      return 'RELEASE';
    case 'work':
      return 'WORK';
    case 'session':
      return 'TRANSCRIPT';
    case 'agent':
      return 'AGENT';
    case 'handoff':
      return 'HANDOFF';
    case 'provider_observation':
      return 'OBSERVED';
    case 'index':
      return 'INDEX';
    default: {
      const unhandled: never = source;
      return unhandled;
    }
  }
}

/** The grade ladder's stroke, from the one grammar every drawn relation shares. */
export function gradeDash(grade: EvidenceGrade): string | undefined {
  return gradeDashArray(grade) || undefined;
}

/**
 * How a served membership basis is graded.
 *
 * This is a fixed presentation of the server's typed basis, not an inference
 * performed here: the daemon names the basis kind, and each kind has exactly
 * one grade under `DESIGN-SYSTEM.md`. A repository/branch reference is a
 * direct Git fact. A Work objective or handoff token is a persisted claim.
 * Session–Git and shared-agent joins are correlations and stay `inferred`
 * until a source authority states them.
 */
export function membershipGrade(basis: DeliveryMembershipBasisV1): EvidenceGrade {
  switch (basis.kind) {
    case 'branch_pull_request_reference':
      return 'exact';
    case 'shared_work_objective':
    case 'explicit_handoff':
      return 'explicit';
    case 'session_git_relation':
    case 'shared_agent':
      return 'inferred';
    default: {
      const unhandled: never = basis;
      return unhandled;
    }
  }
}

export function membershipSourceClass(basis: DeliveryMembershipBasisV1): SourceClass {
  switch (basis.kind) {
    case 'branch_pull_request_reference':
      return 'commit';
    case 'shared_work_objective':
      return 'work';
    case 'explicit_handoff':
      return 'handoff';
    case 'session_git_relation':
      return 'session';
    case 'shared_agent':
      return 'agent';
    default: {
      const unhandled: never = basis;
      return unhandled;
    }
  }
}

/** Whether a basis can relate two distinct pull requests. The branch reference
 * only admits a PR to its own repository head; it never groups PRs. */
export function isCorrelatingBasis(basis: DeliveryMembershipBasisV1): boolean {
  switch (basis.kind) {
    case 'branch_pull_request_reference':
      return false;
    case 'shared_work_objective':
    case 'explicit_handoff':
    case 'session_git_relation':
    case 'shared_agent':
      return true;
    default: {
      const unhandled: never = basis;
      return unhandled;
    }
  }
}

/** The stable identity a correlating basis shares across pull requests. */
export function membershipIdentity(basis: DeliveryMembershipBasisV1): string {
  switch (basis.kind) {
    case 'branch_pull_request_reference':
      return `${basis.branch_ref}@${basis.head_commit_id}`;
    case 'shared_work_objective':
      return basis.work_item_id;
    case 'explicit_handoff':
      return basis.handoff_id;
    case 'session_git_relation':
      return basis.session_id;
    case 'shared_agent':
      return basis.agent_id;
    default: {
      const unhandled: never = basis;
      return unhandled;
    }
  }
}

export function membershipBasisLabel(basis: DeliveryMembershipBasisV1): string {
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

export function membershipBasisDetail(basis: DeliveryMembershipBasisV1): string {
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

/** A real destination the basis identity can pivot to, or `null` when no
 * shipping route honours that identity. Nothing here links to a route that
 * would ignore the parameter. */
export function membershipHref(basis: DeliveryMembershipBasisV1): string | null {
  switch (basis.kind) {
    case 'session_git_relation':
      return `/loom?loomSession=${encodeURIComponent(basis.session_id)}`;
    case 'shared_work_objective':
    case 'explicit_handoff':
    case 'shared_agent':
    case 'branch_pull_request_reference':
      return null;
    default: {
      const unhandled: never = basis;
      return unhandled;
    }
  }
}

export function providerStateKind(state: DeliveryProviderStateV1): DomainStateKind {
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

/** The one sentence the daemon's provider state means for an inbox reader. */
export function providerStateSentence(state: DeliveryProviderStateV1): string {
  switch (state) {
    case 'ready':
      return 'Provider reads completed for the indexed head.';
    case 'partial':
      return 'Provider reads completed with truncation or missing operations.';
    case 'stale':
      return 'Provider reads are retained from an earlier head; the source has moved.';
    case 'rate_limited':
      return 'The provider quota paused reads; retained evidence is shown as-is.';
    case 'failed':
      return 'The provider read failed; nothing is invented in its place.';
    case 'denied':
      return 'The provider refused this identity; no pull request is disclosed.';
    case 'not_published':
      return 'not_published · requires github_read_authority.';
    case 'not_configured':
      return 'No provider read authority is configured for this project.';
    case 'unavailable':
      return 'The provider read authority is not mounted for this project.';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

/** Whether a provider state can serve pull requests at all. Anything else is a
 * typed absence that must not read as an empty inbox. */
export function providerServes(state: DeliveryProviderStateV1): boolean {
  switch (state) {
    case 'ready':
    case 'partial':
    case 'stale':
    case 'rate_limited':
      return true;
    case 'failed':
    case 'denied':
    case 'not_published':
    case 'not_configured':
    case 'unavailable':
      return false;
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

export function providerOutcomeKind(outcome: DeliveryGitHubOutcomeV1): DomainStateKind {
  switch (outcome) {
    case 'complete':
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
    case 'unavailable':
      return 'unavailable';
    default: {
      const unhandled: never = outcome;
      return unhandled;
    }
  }
}

export function coverageKind(coverage: DeliveryInboxCoverageV1): DomainStateKind {
  switch (coverage) {
    case 'complete':
      return 'ready';
    case 'partial':
      return 'partial';
    case 'stale':
      return 'stale';
    case 'denied':
      return 'denied';
    case 'unavailable':
      return 'unavailable';
    case 'unsupported':
      return 'unsupported';
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}
