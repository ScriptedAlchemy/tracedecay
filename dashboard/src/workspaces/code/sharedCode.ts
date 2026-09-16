import type {
  SimilarCoverageV1,
  SimilarMatchClassV1,
  SimilarOccurrenceV1,
} from '../../contracts/generated.ts';

/**
 * SHARED CODE — the two verified exact match classes served by
 * `GET /api/plugins/graph/shared-code/family` (code_read_api.rs).
 *
 * Each class is its own read: the daemon keys conservative and
 * rename-normalized payloads under distinct normalization revisions, and a
 * rename digest is never compared against a conservative one. The view shows
 * them as two peer sections rather than merging their families into one list,
 * so a reader always knows which normalization a copy was verified under.
 *
 * The stitch grammar (mockups/ui-concept-v2/06-code) names one mark per
 * relation kind. Only the two exact classes are served on this route today, so
 * only those two marks are drawn here; the near-clone, containment,
 * difference, and stale marks arrive with the routes that carry their
 * evidence.
 */
export const SHARED_CODE_MATCH_CLASSES: ReadonlyArray<{
  readonly matchClass: SimilarMatchClassV1;
  readonly label: string;
  /** What the daemon verified before calling two bodies members of one family. */
  readonly verified: string;
  /** The stitch mark: one bracket style per relation kind. */
  readonly stitch: 'solid' | 'double';
  readonly stitchLabel: string;
}> = [
  {
    matchClass: 'conservative_exact',
    label: 'Conservative exact',
    verified:
      'identical canonical tokens: comments and formatting ignored, every name, literal, operator, and control-flow order preserved',
    stitch: 'solid',
    stitchLabel: 'solid paired bracket',
  },
  {
    matchClass: 'rename_normalized_exact',
    label: 'Rename-normalized exact',
    verified:
      'identical after reliable local parameter and binding renames; external names, properties, callees, and literals unchanged',
    stitch: 'double',
    stitchLabel: 'double-line bracket',
  },
];

export const SHARED_CODE_FAMILY_ROUTE = '/api/plugins/graph/shared-code/family';

/** Default page size: the route's own `DEFAULT_FAMILY_LIMIT`. */
export const SHARED_CODE_PAGE_LIMIT = 100;

export function sharedFamilyUrl(
  symbolOccurrenceId: string,
  matchClass: SimilarMatchClassV1,
  cursor: string | null,
  limit = SHARED_CODE_PAGE_LIMIT,
): string {
  const params = new URLSearchParams();
  params.set('symbol_occurrence_id', symbolOccurrenceId);
  params.set('match_class', matchClass);
  params.set('limit', String(limit));
  if (cursor !== null) params.set('cursor', cursor);
  return `${SHARED_CODE_FAMILY_ROUTE}?${params.toString()}`;
}

/** How the result's own coverage reads to a person. The four statuses are the
 * daemon's (`SimilarCoverageV1`); nothing here infers coverage from counts. */
export type SharedCodeCoverageReading =
  | { readonly kind: 'complete' }
  | {
      readonly kind: 'partial';
      readonly sentence: string;
    }
  | {
      readonly kind: 'excluded';
      readonly title: string;
      readonly sentence: string;
    };

export function readSharedCodeCoverage(coverage: SimilarCoverageV1): SharedCodeCoverageReading {
  switch (coverage.status) {
    case 'complete':
      return { kind: 'complete' };
    case 'partial':
      return {
        kind: 'partial',
        sentence:
          'Not every member of this body\'s families is on this page. What is listed is verified; the family headers say whether a further page exists.',
      };
    case 'excluded_too_small':
      return {
        kind: 'excluded',
        title: 'Excluded from automatic discovery',
        sentence: `This body is under the ${coverage.minimum_tokens.toLocaleString()}-token minimum, so no family was searched for it. That is an exclusion, not a finding of zero copies.`,
      };
    case 'excluded_incomplete_tokenization':
      return {
        kind: 'excluded',
        title: 'Excluded: tokenization incomplete',
        sentence:
          'The extractor could not tokenize this body completely, so it has no digest to compare. That is an exclusion, not a finding of zero copies.',
      };
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

/** The leading characters of a content digest, enough to tell two apart on a
 * page without printing sixty-four of them. The full digest stays in `title`. */
export function shortDigest(digest: string): string {
  const separator = digest.indexOf(':');
  const hex = separator === -1 ? digest : digest.slice(separator + 1);
  return hex.length <= 12 ? hex : hex.slice(0, 12);
}

/** `path` and the exact byte span of the body: the occurrence's source
 * identity as the daemon states it. Line numbers are mutable and are not part
 * of that identity, so none is invented here. */
export function describeOccurrence(occurrence: SimilarOccurrenceV1): string {
  return `${occurrence.path} · bytes ${occurrence.body_span.start_byte.toLocaleString()}–${occurrence.body_span.end_byte.toLocaleString()}`;
}
