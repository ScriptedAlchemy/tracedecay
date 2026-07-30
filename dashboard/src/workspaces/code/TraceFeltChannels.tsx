/**
 * The felt half of the sensory contract.
 *
 * The key beside it covers what the field DRAWS. This covers what it does to
 * the hand: weight, tension, texture, warmth, pulse. Two of the five are driven
 * on this route and three are inert, and a surface that animates two channels
 * while saying nothing about the other three is quietly claiming five
 * measurements it does not have. Each row therefore prints its state and its
 * static equivalent — the reading that survives when motion is reduced.
 *
 * It is its own module because that claim is about the ROUTE, not about the
 * picture: which channels a payload can drive changes when the payload changes,
 * and `sensoryChannels` derives all of it from the payload's own field names, so
 * a producer that starts sending complexity or churn lights the corresponding
 * row up without an edit here.
 */
import { sensoryChannels } from '../../viz/trace/model.ts';
import { cn } from '../../ui/cn';
import type { SensoryChannelState, TraceModel } from '../../viz/trace/types.ts';

/** How a sensory channel's state prints, and in which ink. */
export function channelState(state: SensoryChannelState): { label: string; tone: string } {
  switch (state) {
    case 'measured':
      return { label: 'measured', tone: 'text-text-secondary' };
    case 'not-on-this-wire':
      return { label: 'not on this wire', tone: 'text-state-unknown' };
    case 'coarser-scope':
      return { label: 'coarser scope', tone: 'text-state-unknown' };
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

export function TraceFeltChannels({ model }: { model: TraceModel }) {
  return (
    <div className="flex flex-col gap-1 border-t border-edge-subtle pt-2">
      <div className="flex items-center gap-1.5">
        <h3 className="td-legend">Felt channels</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <dl className="grid grid-cols-1 gap-x-3 gap-y-1.5 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-5">
        {sensoryChannels(model).map((channel) => {
          const state = channelState(channel.state);
          return (
            <div key={channel.feel} className="flex min-w-0 flex-col gap-0.5">
              <dt className="td-legend whitespace-normal normal-case tracking-normal text-text-primary">
                {channel.feel}
              </dt>
              <dd className="flex min-w-0 flex-col gap-0.5">
                <span className={cn('td-value text-2xs', state.tone)}>{state.label}</span>
                <span className="text-3xs leading-snug text-text-muted">
                  {channel.measurement} · still: {channel.staticEquivalent}
                </span>
                <span className="text-3xs leading-snug text-text-secondary">{channel.note}</span>
              </dd>
            </div>
          );
        })}
      </dl>
    </div>
  );
}
