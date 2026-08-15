import type { ListedTaskHandoffV1 } from '../../contracts/generated.ts';
import { ReadFailure } from '../../ui/LegacyStates.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { handoffTargetLabel, type HandoffTokenReading } from './handoffTokens.ts';

function TokenRow({ handoff }: { handoff: ListedTaskHandoffV1 }) {
  return (
    <li
      className="flex min-w-0 flex-col border-b border-edge-subtle py-1 last:border-b-0"
      data-handoff-token={handoff.token_digest}
      data-handoff-token-state={handoff.state}
    >
      <span className="truncate text-2xs text-text-primary">{handoffTargetLabel(handoff)}</span>
      <span className="truncate text-3xs text-text-muted">
        {handoff.kind} · issued {formatMicrosUtc(handoff.issued_at)} · expires{' '}
        {formatMicrosUtc(handoff.expires_at)}
        {handoff.consumed_at != null ? ` · redeemed ${formatMicrosUtc(handoff.consumed_at)}` : ''}
      </span>
    </li>
  );
}

/**
 * Outstanding and dropped handoff tokens for one session.
 *
 * The plate beside this one reads the work graph, which records handoffs that
 * happened. This one reads the daemon's grant store, which is the only place a
 * handoff that was offered and never taken up leaves a trace at all.
 *
 * The empty case is the one that has to be said carefully. The daemon answers
 * this route with exactly the grants the caller could itself redeem — same
 * session, same scope, same recipient principal. So nothing here is ever
 * evidence that no tokens exist; it is evidence that none was addressed to this
 * reader. Those are different facts and the surface prints the second one.
 */
export function AgentHandoffTokens({ reading }: { reading: HandoffTokenReading }) {
  if (reading.state === 'pending') {
    return (
      <p className="text-2xs text-text-muted" data-handoff-tokens="pending">
        reading the handoff-token frontier
      </p>
    );
  }
  if (reading.state === 'unasked') {
    return (
      <p className="text-2xs leading-relaxed text-text-muted" data-handoff-tokens="unasked">
        {reading.detail}
      </p>
    );
  }
  if (reading.state === 'refused') {
    return <ReadFailure label="Handoff tokens unavailable" detail={reading.detail} />;
  }

  const { outstanding, lapsed, redeemed } = reading;
  const total = outstanding.length + lapsed.length + redeemed.length;

  return (
    <div className="flex min-w-0 flex-col gap-2" data-handoff-tokens={total}>
      <p className="text-xs leading-relaxed text-text-primary">
        {total === 0 ? (
          <>
            No handoff token in <span className="td-value">{reading.sessionId}</span> is addressed
            to this reader. The daemon answers this route with only the grants the caller could
            itself redeem, so this is a statement about who the reader is — not evidence that no
            handoff tokens exist.
          </>
        ) : (
          <>
            <span className="td-value">{outstanding.length}</span> outstanding,{' '}
            <span className="td-value">{lapsed.length}</span> lapsed unredeemed, and{' '}
            <span className="td-value">{redeemed.length}</span> redeemed, in{' '}
            <span className="td-value">{reading.sessionId}</span> as of{' '}
            {formatMicrosUtc(reading.observedAtMicros)}.
          </>
        )}
      </p>

      {lapsed.length > 0 ? (
        <section aria-label="Lapsed handoff tokens" className="flex min-w-0 flex-col gap-1">
          <h3 className="td-legend text-text-secondary">Lapsed unredeemed</h3>
          <p className="text-3xs leading-relaxed text-text-muted">
            Offered and never taken up. These leave no work-graph record at all, so this is the
            only surface on which a dropped handoff is visible.
          </p>
          <ul className="flex min-w-0 flex-col">
            {lapsed.map((handoff) => (
              <TokenRow key={handoff.token_digest} handoff={handoff} />
            ))}
          </ul>
        </section>
      ) : null}

      {outstanding.length > 0 ? (
        <section aria-label="Outstanding handoff tokens" className="flex min-w-0 flex-col gap-1">
          <h3 className="td-legend text-text-secondary">Outstanding</h3>
          <ul className="flex min-w-0 flex-col">
            {outstanding.map((handoff) => (
              <TokenRow key={handoff.token_digest} handoff={handoff} />
            ))}
          </ul>
        </section>
      ) : null}

      {redeemed.length > 0 ? (
        <section aria-label="Redeemed handoff tokens" className="flex min-w-0 flex-col gap-1">
          <h3 className="td-legend text-text-secondary">Redeemed</h3>
          <ul className="flex min-w-0 flex-col">
            {redeemed.map((handoff) => (
              <TokenRow key={handoff.token_digest} handoff={handoff} />
            ))}
          </ul>
        </section>
      ) : null}

      {reading.truncated ? (
        <p className="text-2xs leading-relaxed text-state-unavailable" data-handoff-tokens-truncated="true">
          The enumeration stopped at its ceiling, so the counts above describe a prefix of the
          frontier rather than all of it.
        </p>
      ) : null}
    </div>
  );
}
