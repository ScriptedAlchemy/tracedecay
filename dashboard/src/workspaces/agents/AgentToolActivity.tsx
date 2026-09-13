import { MeterRow } from '../../ui/instrument.tsx';
import { logFraction } from '../../viz/scale.ts';
import {
  agentToolActivity,
  rankedCounts,
  toolCallSplit,
  toolCallsPerMessage,
  type ToolActivityRead,
} from './activity.ts';

/**
 * Tool activity: how much of the window was tool use, of what kind, and by
 * whom.
 *
 * The page's neighbouring plate already ranks WHICH tools were called. This one
 * answers the questions that ranking cannot: a rank has no denominator, so it
 * cannot say whether tool use was most of what happened or a rounding error,
 * and it carries no agent, so it cannot say who did any of it.
 *
 * Two absences are stated rather than filled. The MCP/other split is a
 * subtraction and is only drawn when both counts arrived. The per-agent
 * attribution comes from `recent_hooks`, which is a recent suffix of the hook
 * tape and never a total — so it is captioned as a suffix, and when the fold
 * serves none the surface says the fold attributed nothing rather than that no
 * agent used a tool.
 */
export function AgentToolActivity({ payload }: { payload: ToolActivityRead }) {
  const split = toolCallSplit(payload);
  const perMessage = toolCallsPerMessage(payload.ratios);
  const categories = rankedCounts(payload.by_tool_category ?? [], 'tool_category');
  const tools = rankedCounts(payload.by_tool ?? [], 'tool_name');
  const attribution = agentToolActivity(payload.recent_hooks ?? []);
  const truncated = payload.hook_window?.truncated === true;

  return (
    <div className="flex min-w-0 flex-col gap-3" data-agent-tool-activity="read">
      <p className="text-xs leading-relaxed text-text-primary">
        {split.total == null ? (
          <>
            The fold served no tool-call total for this window, so what share of it was tool use
            is unknown.{' '}
            {split.mcp == null
              ? 'It served no MCP tool-call count either.'
              : `It did count ${split.mcp.toLocaleString()} MCP tool calls, which is a floor and not a share.`}
          </>
        ) : (
          <>
            <span className="td-value">{split.total.toLocaleString()}</span> tool{' '}
            {split.total === 1 ? 'call' : 'calls'} in the window
            {perMessage != null ? (
              <>
                {' '}
                — <span className="td-value">{perMessage.toFixed(2)}</span> per message, as the
                fold reports it
              </>
            ) : null}
            .
          </>
        )}
      </p>

      {split.contradiction ? (
        <p className="text-2xs leading-relaxed text-state-conflicting" data-agent-tool-split="contradiction">
          The fold reports {split.mcp?.toLocaleString()} MCP tool calls inside{' '}
          {split.total?.toLocaleString()} tool calls. The two disagree, so no remainder is drawn
          from them — a non-MCP figure subtracted here would be this build inventing a number to
          make the totals agree.
        </p>
      ) : split.total != null && split.mcp != null && split.other != null ? (
        <figure className="flex min-w-0 flex-col gap-1.5" data-agent-tool-split="drawn">
          <figcaption className="td-legend">
            how the {split.total.toLocaleString()} calls divide · share of the total
          </figcaption>
          <MeterRow
            label="through MCP"
            fraction={split.total > 0 ? split.mcp / split.total : null}
            value={split.mcp.toLocaleString()}
          />
          <MeterRow
            label="not through MCP"
            fraction={split.total > 0 ? split.other / split.total : null}
            value={split.other.toLocaleString()}
          />
          {split.tracedecay != null ? (
            <figcaption className="text-3xs leading-relaxed text-text-muted">
              {split.tracedecay.toLocaleString()} of them were calls into this system's own
              tools, counted separately by the fold and overlapping the rails above rather than
              adding to them.
            </figcaption>
          ) : null}
        </figure>
      ) : (
        <p className="text-2xs leading-relaxed text-text-muted" data-agent-tool-split="unsplit">
          The window's tool calls are not split here: the split is the tool total less the MCP
          total, and{' '}
          {split.total == null && split.mcp == null
            ? 'the fold served neither'
            : split.total == null
              ? 'the fold served no tool total'
              : 'the fold served no MCP total'}
          .
        </p>
      )}

      {categories.length > 0 ? (
        <figure className="flex min-w-0 flex-col gap-1.5">
          <figcaption className="td-legend">
            by tool category
            {split.total != null ? ` · share of ${split.total.toLocaleString()}` : ' · tool total unreported'}
          </figcaption>
          {categories.map((row) => (
            <MeterRow
              key={row.label}
              label={row.label}
              fraction={split.total != null && split.total > 0 ? row.count / split.total : null}
              value={row.count.toLocaleString()}
            />
          ))}
        </figure>
      ) : (
        <p className="text-2xs leading-relaxed text-text-muted">
          The fold served no tool categories for this window.
        </p>
      )}

      <section aria-label="Tool activity by agent" className="flex min-w-0 flex-col gap-1.5">
        <h3 className="td-legend text-text-secondary">By agent</h3>
        {attribution.agents.length === 0 ? (
          <p className="text-2xs leading-relaxed text-text-muted" data-agent-attribution="none">
            The diagnostics fold served no hook row naming an agent, so not one of the tool calls
            counted above is attributed to an agent here.{' '}
            {attribution.unattributed > 0
              ? `${attribution.unattributed.toLocaleString()} hook ${attribution.unattributed === 1 ? 'row' : 'rows'} arrived without one.`
              : 'It served no hook rows at all.'}{' '}
            This is the tape carrying no attribution, not a measurement that agents called
            nothing.
          </p>
        ) : (
          <>
            <p className="text-3xs leading-relaxed text-text-muted">
              From the hook tape the fold served, which is {truncated ? 'a recent suffix' : 'the window it scanned'} and
              never a total — these counts rank agents against each other on that suffix and are
              floors, not totals.
            </p>
            <ul className="flex min-w-0 flex-col" data-agent-attribution={attribution.agents.length}>
              {attribution.agents.map((row) => (
                <li
                  key={row.agent}
                  className="flex flex-col gap-0.5 border-b border-edge-subtle py-1.5 last:border-b-0"
                  data-agent-attribution-agent={row.agent}
                >
                  <MeterRow
                    label={row.agent}
                    title={row.agent}
                    fraction={logFraction(row.calls, attribution.agents[0]?.calls ?? 0)}
                    value={row.calls.toLocaleString()}
                  />
                  <span className="truncate text-3xs text-text-muted">
                    {row.sessions > 0
                      ? `${row.sessions} ${row.sessions === 1 ? 'session' : 'sessions'} · `
                      : 'session unrecorded · '}
                    {row.tools.length === 0
                      ? 'no tool named on these rows'
                      : row.tools
                          .slice(0, 4)
                          .map((tool) => `${tool.label} ${tool.count}`)
                          .join(' · ')}
                    {row.tools.length > 4 ? ` · +${row.tools.length - 4} more` : ''}
                  </span>
                </li>
              ))}
            </ul>
            {attribution.unattributed > 0 ? (
              <p className="text-3xs leading-relaxed text-text-muted">
                {attribution.unattributed.toLocaleString()} further hook{' '}
                {attribution.unattributed === 1 ? 'row names' : 'rows name'} no agent and{' '}
                {attribution.unattributed === 1 ? 'is' : 'are'} not ranked above — an unnamed row
                drawn beside named agents would read as an agent called nothing.
              </p>
            ) : null}
          </>
        )}
      </section>

      {tools.length > 0 ? (
        <p className="text-3xs leading-relaxed text-text-muted">
          The fold names {tools.length.toLocaleString()} distinct tools across every transport,
          ranked on the plate beside this one for the MCP subset.
        </p>
      ) : null}
    </div>
  );
}
