import { StateChip } from '../../ui/StateChip.tsx';
import type { AuthorityState } from './authorityRegister.ts';

/**
 * Five authorities, five cells, one ruled bar. Each cell is the state one
 * authority reported and the figure or reason it reported it with. There is
 * deliberately no total, no percentage and no summary lamp: the register's
 * whole job is to keep independent readings from being read as one.
 */
export function AgentAuthorityRegister({ authorities }: { authorities: readonly AuthorityState[] }) {
  return (
    <dl
      aria-label="Independent authorities"
      className="td-raised relative grid grid-cols-1 border-y border-edge-subtle sm:grid-cols-2 lg:grid-cols-5"
      data-agent-authorities={authorities.length}
    >
      {authorities.map((authority) => (
        <div
          key={authority.id}
          className="flex min-w-0 flex-col gap-1.5 border-l border-edge-subtle px-3 py-2.5 first:border-l-0 max-sm:border-l-0 max-sm:border-t max-sm:first:border-t-0"
          data-agent-authority={authority.id}
          data-agent-authority-state={authority.kind}
        >
          <dt className="flex items-center gap-2">
            <span className="td-legend truncate">{authority.label}</span>
            <span aria-hidden className="td-rule" />
            {authority.source ? (
              <span className="td-value truncate text-3xs text-text-muted">{authority.source}</span>
            ) : null}
          </dt>
          <dd className="flex min-w-0 flex-col gap-1">
            <StateChip kind={authority.kind} detail={authority.detail} />
          </dd>
        </div>
      ))}
    </dl>
  );
}
