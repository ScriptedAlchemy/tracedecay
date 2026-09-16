import {
  assertNever,
  type CodeCloneIndexObservationV1,
  type CodeCloneIndexStatusV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

const cloneIndexStates = {
  unavailable: 'unavailable',
  backfilling: 'loading',
  partial: 'partial',
  ready: 'ready',
  stale: 'stale',
} as const satisfies Record<CodeCloneIndexStatusV1['state'], DomainStateKind>;

export function cloneIndexState(status: CodeCloneIndexStatusV1): DomainStateKind {
  return cloneIndexStates[status.state];
}

export function cloneIndexObservation(
  status: CodeCloneIndexStatusV1,
): CodeCloneIndexObservationV1 | null {
  return 'observation' in status ? status.observation : null;
}

export function cloneIndexDetail(status: CodeCloneIndexStatusV1): string {
  switch (status.state) {
    case 'unavailable':
      return status.reason;
    case 'backfilling':
      return 'clone fingerprints are backfilling from the sealed lexical source';
    case 'partial':
      return status.omission_reasons.join(' · ');
    case 'ready':
      return 'exact and near-fingerprint postings are ready';
    case 'stale':
      return status.reason;
    default:
      return assertNever(status);
  }
}

export function formatCloneDuration(micros: number | null | undefined): string {
  if (micros == null) return 'unavailable';
  if (micros < 1_000) return `${micros}µs`;
  if (micros < 1_000_000) return `${(micros / 1_000).toFixed(2)}ms`;
  return `${(micros / 1_000_000).toFixed(2)}s`;
}
