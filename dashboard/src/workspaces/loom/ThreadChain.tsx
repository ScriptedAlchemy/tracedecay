import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { StateChip } from '../../ui/StateChip';
import { Legend, Meter, Panel } from '../../ui/instrument.tsx';
import { formatCount } from '../../ui/format.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { formatDuration, formatMoment } from './tracks.ts';
import { summarizeChain, type PlacedThread } from './weave.ts';
import { LoomChainPayloadSchema } from './contracts.ts';

/**
 * The selected thread's chain: prompt → turns → tools.
 *
 * This is the "selecting a thread isolates its chain" half of the Loom, and it
 * is also where the plan's chain — prompt → tools → edits → commits — visibly
 * runs out of wire. The first two links are measured and drawn; the last two
 * are terminated with a named absence rather than trailing off.
 *
 * One further honesty point drives the whole layout: the LCM session endpoint
 * serves a `timestamp` on every message and it is null on every message of
 * every session on the real profile. So the chain is ordered by the store's
 * `ordinal` and presented as a SEQUENCE, never as a timeline — no elapsed
 * times, no gaps, no per-turn axis. A time-looking chain built on ordinals
 * would be the exact kind of decorative fiction this surface exists to avoid.
 */
export function ThreadChain({ thread }: { thread: PlacedThread | null }) {
  const chain = useLegacy(
    ['loom', 'chain', thread?.id ?? 'none'],
    `/api/plugins/hermes-lcm/session/${encodeURIComponent(thread?.id ?? '')}?limit=200`,
    LoomChainPayloadSchema,
    { enabled: thread != null },
  );

  if (!thread) {
    return (
      <Panel legend="Thread">
        <p className="text-2xs leading-relaxed text-text-muted">
          Select a thread — on the weave or in the table — to isolate it and
          pull its chain of turns and tool calls from the session store.
        </p>
      </Panel>
    );
  }

  return (
    <Panel legend="Thread" footer={<ChainTerminus />}>
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-1">
          <span className="text-xs font-medium leading-snug text-text-primary">
            {thread.label}
          </span>
          <span className="td-value truncate text-3xs text-text-muted">
            {thread.id}
          </span>
        </div>

        <dl className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-2xs">
          <Fact label="host" value={thread.host} />
          <Fact label="started" value={formatMoment(thread.start)} />
          <Fact
            label="extent"
            value={
              thread.end != null
                ? formatDuration(thread.end - thread.start)
                : 'not served'
            }
            muted={thread.end == null}
          />
          <Fact label="kind" value={thread.isSubagent ? 'subagent' : 'session'} />
        </dl>

        {thread.models.length > 0 ? (
          <div className="flex flex-col gap-1">
            <Legend>models</Legend>
            <div className="flex flex-wrap gap-1">
              {thread.models.map((model) => (
                <span
                  key={model}
                  className="td-value border border-edge-subtle px-1.5 py-0.5 text-3xs text-text-secondary"
                >
                  {model}
                </span>
              ))}
            </div>
          </div>
        ) : null}

        <LegacyBoundary title="Chain" pending={chain.isPending} result={chain.data}>
          {(data) => {
            if (data.exists === false) {
              return (
                <StateChip
                  kind="unknown"
                  detail="no transcript recorded for this session"
                />
              );
            }
            const summary = summarizeChain(
              data.messages ?? [],
              data.counts,
              data.has_more_messages === true,
            );
            if (summary.steps.length === 0) {
              return (
                <StateChip kind="complete_zero_findings" detail="session holds no turns" />
              );
            }
            const toolCeiling = summary.tools.reduce(
              (max, tool) => Math.max(max, tool.count),
              0,
            );
            return (
              <div className="flex flex-col gap-3">
                <div className="flex flex-col gap-1">
                  <Legend
                    trailing={
                      <span
                        className="td-value shrink-0 text-3xs text-text-muted"
                        data-cell="numeric"
                      >
                        {formatCount(summary.messageCount)} turns
                      </span>
                    }
                  >
                    composition
                  </Legend>
                  <div className="flex flex-wrap gap-x-3 gap-y-1">
                    {summary.roles.map((role) => (
                      <span key={role.role} className="text-3xs text-text-secondary">
                        <span className="tabular-nums text-text-primary">
                          {role.count}
                        </span>{' '}
                        {role.role}
                      </span>
                    ))}
                  </div>
                </div>

                <div className="flex flex-col gap-1.5">
                  <Legend>tools invoked</Legend>
                  {summary.tools.length === 0 ? (
                    <StateChip
                      kind="complete_zero_findings"
                      detail="no turn named a tool"
                    />
                  ) : (
                    <ul className="flex flex-col gap-1">
                      {summary.tools.map((tool) => (
                        <li key={tool.tool} className="flex items-center gap-2">
                          <span className="min-w-0 flex-1 truncate text-3xs text-text-secondary">
                            {tool.tool}
                          </span>
                          <Meter
                            fraction={toolCeiling > 0 ? tool.count / toolCeiling : null}
                            className="w-12 shrink-0"
                          />
                          <span
                            className="td-value w-6 shrink-0 text-right text-3xs text-text-primary"
                            data-cell="numeric"
                          >
                            {tool.count}
                          </span>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>

                <div className="flex flex-col gap-1.5">
                  <Legend
                    trailing={
                      <span className="shrink-0 text-3xs text-text-muted">
                        {summary.timestamped ? 'time-ordered' : 'ordinal order'}
                      </span>
                    }
                  >
                    sequence
                  </Legend>
                  {summary.timestamped ? null : (
                    <p className="text-3xs leading-relaxed text-text-muted">
                      The store served no timestamp on any turn of this session,
                      so this is the recorded order — not a timeline. No elapsed
                      time between turns is known.
                    </p>
                  )}
                  <ol className="max-h-56 overflow-auto border border-edge-subtle">
                    {summary.steps.map((step, index) => (
                      <li
                        key={step.id}
                        className="flex gap-2 border-b border-edge-subtle px-2 py-1 last:border-b-0"
                      >
                        <span
                          className="td-value w-5 shrink-0 text-right text-3xs text-text-muted"
                          data-cell="numeric"
                        >
                          {index + 1}
                        </span>
                        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                          <span className="flex items-baseline gap-1.5">
                            <span className="td-legend shrink-0 text-text-secondary">
                              {step.role}
                            </span>
                            {step.tool ? (
                              <span className="td-value truncate text-3xs text-text-primary">
                                {step.tool}
                              </span>
                            ) : null}
                          </span>
                          {step.excerpt ? (
                            <span className="truncate text-3xs text-text-muted">
                              {step.excerpt}
                            </span>
                          ) : null}
                        </span>
                      </li>
                    ))}
                  </ol>
                  {summary.truncated ? (
                    <span className="text-3xs text-text-muted">
                      First {summary.steps.length} of{' '}
                      {formatCount(summary.messageCount)} turns — the store has
                      more than this page.
                    </span>
                  ) : null}
                </div>
              </div>
            );
          }}
        </LegacyBoundary>
      </div>
    </Panel>
  );
}

/** Where the chain stops, and why. The plan's chain continues into edits and
 * commits; the wire does not, so the rail terminates explicitly instead of
 * simply ending and letting the reader assume there was nothing there. */
function ChainTerminus() {
  return (
    <div className="flex flex-col gap-1">
      <span className="td-legend text-text-secondary">→ edits → commits</span>
      <StateChip kind="unsupported" detail="no session→file or session→commit route" />
    </div>
  );
}

function Fact({
  label,
  value,
  muted,
}: {
  label: string;
  value: string;
  muted?: boolean;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="td-legend">{label}</dt>
      <dd
        className={
          muted
            ? 'truncate text-3xs text-text-muted'
            : 'truncate text-3xs text-text-secondary'
        }
      >
        {value}
      </dd>
    </div>
  );
}
