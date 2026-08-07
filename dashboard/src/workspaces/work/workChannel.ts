import type { DomainStateKind } from '../../ui/StateChip.tsx';

/**
 * The channel vocabulary the four Work projections speak.
 *
 * A channel is never estimated to fill a gap, and a channel that goes live is
 * live because a read proved it rather than because a read arrived. Plan 11c is
 * explicit: attempt counts, queue ages and wall-times are real or absent, and a
 * degenerate distribution is said rather than drawn.
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

/**
 * The measurements plan 11c asks a projection to encode that NO contract in
 * this build carries, however many routes are mounted.
 *
 * Each is named for the measurement rather than for the route that would supply
 * it, because the gap is in the read model rather than in the transport. That
 * distinction is what keeps this list short and honest, and it is why three of
 * its former members are gone rather than restated: effort, concurrency and
 * churn were schema gaps until `WorkProductProjectionBundleV1` reached the wire,
 * and a build that kept reporting them as `unsupported_schema` would be telling
 * a reader it cannot do something it can. When the graph read has not answered
 * those channels carry the READ's state instead, through `graphChannelGap` —
 * exactly as the attempt-fed channels carry theirs through `attemptChannelGap`.
 *
 * What is left are the two absences no mounted read closes. Both are stated in
 * `workViewsModel.ts`'s module doc; both are about a measurement that does not
 * exist rather than one that has not arrived.
 */
export type WorkChannelGap = 'wall_clock' | 'observed_order';

export function channelGap(gap: WorkChannelGap): {
  state: DomainStateKind;
  detail: string;
} {
  switch (gap) {
    case 'wall_clock':
      return {
        state: 'unsupported_schema',
        detail:
          'no span can be drawn: an attempt records the instant it was observed to finish, and nothing anywhere records when it started — WorkLeaseFenceV1 is {epoch, lease_id} and WorkAttemptProgressV1 is {completed, total}, and the work-product graph read does not add one, because its runtime projection carries an attempt identity and a state and no instant at all, so every mark has an end and never a width',
      };
    case 'observed_order':
      return {
        state: 'unsupported_schema',
        detail:
          "no order of execution is readable here: the snapshot carries no timestamp, and the work-product graph read answers declared causal candidates rather than an observed sequence, so nothing binds a task's completion to the instant another task finished — the weave's terminal order, read from the attempt list, is the nearest measurement this build has and it ranks attempts rather than tasks",
      };
    default: {
      const unhandled: never = gap;
      return unhandled;
    }
  }
}

export function absentChannel(gap: WorkChannelGap): WorkChannel<never> {
  const { state, detail } = channelGap(gap);
  return { available: false, state, detail };
}
