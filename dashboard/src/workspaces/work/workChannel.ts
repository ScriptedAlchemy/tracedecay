import type { DomainStateKind } from '../../ui/StateChip.tsx';

/**
 * The channel vocabulary the Work projections speak.
 *
 * A channel is never estimated to fill a gap, and a channel that goes live is
 * live because a read proved it rather than because a read arrived. Attempt
 * counts, queue ages and wall-times are real or absent, and a degenerate
 * distribution is said rather than drawn.
 */

/**
 * One measurement channel, either proved or explained.
 *
 * There is no third case and no default value. A view that renders
 * `available: false` renders the state and the sentence, so an absence can
 * never be mistaken for a zero.
 */
export type WorkChannel<T> =
  | { readonly available: true; readonly value: T }
  | { readonly available: false; readonly state: DomainStateKind; readonly detail: string };
