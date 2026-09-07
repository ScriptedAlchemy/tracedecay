/**
 * The one formatter for a content-token reading and its provenance.
 *
 * The transcript inspector, the Loom thread chain and the sessions timeline
 * all print the same `~N tokens · o200k approximate` sentence; written three
 * times the copies had already begun to word themselves differently, so the
 * measured case lives here once. A count whose provenance the store disclaims
 * (`unavailable`) or never recorded (`null` on the wire) is not a measurement
 * to print — those return `null`, and each surface words its own absence.
 */
import {
  assertNever,
  type LcmTokenCountProvenanceV1,
} from '../../contracts/generated.ts';

export function tokenCountLabel(
  tokenCount: number | null | undefined,
  provenance: LcmTokenCountProvenanceV1 | null | undefined,
): string | null {
  if (tokenCount == null || provenance == null) return null;
  switch (provenance) {
    case 'o200k_approximate':
      return `~${tokenCount.toLocaleString()} tokens · o200k approximate`;
    case 'unavailable':
      return null;
    default:
      return assertNever(provenance);
  }
}
