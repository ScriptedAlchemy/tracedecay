import type { DashboardEnvelopeV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { envelopeReadState, type ReadState } from '../../ui/ReadSection.tsx';

/**
 * The typed refusals of the two code-read routes (`DashboardCodeReadErrorV1::
 * reason()` in code_read_api.rs), worded for a reader. A refusal the daemon
 * answered with a `null` payload reaches the browser as a transport outcome
 * whose detail is this reason string; the chip would otherwise print the
 * identifier verbatim.
 *
 * Every sentence says what the state *is*; none of them substitutes a result.
 */
const CODE_READ_REASONS: Readonly<Record<string, string>> = {
  selected_source_not_found:
    'The selected occurrence is not in the retained clone index: it is stale, unknown, or outside the retained scope.',
  invalid_request: 'The request was malformed; the daemon refused it before reading anything.',
  selected_revision_changed:
    'A selected reference no longer points at its expected revision. The comparison was not made against a different commit.',
  code_read_authority_unavailable: 'The code-read authority is not mounted on this daemon.',
  code_generation_unavailable: 'No sealed code-index generation is available for this scope yet.',
  code_read_capacity_unavailable:
    'A retained generation exceeds the bounded-read limits, so the daemon refused rather than read it partially.',
  code_index_reset_required:
    'The clone index reports corruption and requires an explicit reset; nothing was read.',
  request_cancelled: 'The read was cancelled before it finished.',
  request_timed_out: 'The read did not finish within its deadline.',
  code_read_failed: 'The read failed inside the daemon.',
};

export function describeCodeReadReason(reason: string | undefined): string | undefined {
  if (reason === undefined) return undefined;
  return CODE_READ_REASONS[reason] ?? reason;
}

/** `envelopeReadState` with the route's reason vocabulary worded. */
export function codeReadState<T>(
  pending: boolean,
  result: EnvelopeResult<T> | undefined,
  details: { loading: string; transport: string },
): ReadState<DashboardEnvelopeV1<T>> {
  const state = envelopeReadState(pending, result, details);
  if (state.kind === 'blocked' && result?.outcome === 'transport') {
    return { ...state, detail: describeCodeReadReason(result.detail) ?? details.transport };
  }
  return state;
}
