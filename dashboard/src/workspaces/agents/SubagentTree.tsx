import type { AnalyticsSubagentNodeV1, AnalyticsSubagentTreePayloadV1 } from '../../contracts/generated.ts';
import {
  groupSubagentTrees,
  subagentElapsedSeconds,
  subagentLabel,
  subagentTreeCensus,
} from './subagentTree.ts';

/** How far each depth step is indented, in rem. */
const INDENT_REM = 0.85;

function LinkNote({ node }: { node: AnalyticsSubagentNodeV1 }) {
  switch (node.link) {
    case 'root':
      return null;
    case 'linked':
      return null;
    case 'missing_parent':
      return (
        <span className="text-3xs text-state-unavailable" data-subagent-link="missing_parent">
          parent {node.parent_session_id} not in this reading — drawn at the margin, but it is a
          cut edge and not a root
        </span>
      );
    case 'cycle':
      return (
        <span className="text-3xs text-state-conflicting" data-subagent-link="cycle">
          its parent chain closes on itself, so no root reaches it — surfaced here rather than
          dropped from the count
        </span>
      );
    default: {
      const unhandled: never = node.link;
      return unhandled;
    }
  }
}

/**
 * The delegation tree, drawn from the daemon's pre-order reading.
 *
 * Every node in the payload is drawn. Nothing is filtered for tidiness: a
 * session omitted here would silently shrink the delegation the page is
 * reporting, and the three abnormal link kinds are exactly the ones a reader
 * needs to see, because they are where the store's picture is incomplete.
 */
export function SubagentTree({ payload }: { payload: AnalyticsSubagentTreePayloadV1 }) {
  const census = subagentTreeCensus(payload);

  if (census.nodes === 0) {
    return (
      <p className="text-2xs leading-relaxed text-text-muted" data-subagent-tree="empty">
        The session store holds no session for this project, so there is no delegation tree to
        draw. This is an empty store, not a project whose agents delegated nothing.
      </p>
    );
  }

  const groups = groupSubagentTrees(payload.nodes);

  return (
    <div className="flex min-w-0 flex-col gap-2" data-subagent-tree={census.nodes}>
      <p className="text-xs leading-relaxed text-text-primary">
        {census.flat ? (
          <>
            <span className="td-value">{census.sessionsRead.toLocaleString()}</span> sessions, and
            not one parent/child edge among them. Every session here began on its own — this is a
            measured absence of delegation, not an unread tree.
          </>
        ) : (
          <>
            <span className="td-value">{census.edges.toLocaleString()}</span> delegation{' '}
            {census.edges === 1 ? 'edge' : 'edges'} across{' '}
            <span className="td-value">{census.sessionsRead.toLocaleString()}</span> sessions,{' '}
            {census.maxDepth === 0 ? 'all at one level' : (
              <>
                nested <span className="td-value">{census.maxDepth}</span> deep at the furthest
              </>
            )}
            .
          </>
        )}
      </p>

      {census.missingParents > 0 || census.cycles > 0 ? (
        <p className="text-2xs leading-relaxed text-text-muted" data-subagent-tree-caveats="present">
          {census.missingParents > 0 ? (
            <>
              {census.missingParents.toLocaleString()}{' '}
              {census.missingParents === 1 ? 'session names a parent' : 'sessions name parents'}{' '}
              this reading does not hold, so {census.missingParents === 1 ? 'its' : 'their'} depth
              is measured from a cut edge rather than from a real root.{' '}
            </>
          ) : null}
          {census.cycles > 0 ? (
            <>
              {census.cycles.toLocaleString()}{' '}
              {census.cycles === 1 ? 'session sits' : 'sessions sit'} on a parent chain that closes
              on itself and {census.cycles === 1 ? 'is' : 'are'} reachable from no root.
            </>
          ) : null}
        </p>
      ) : null}

      {census.truncated ? (
        <p className="text-2xs leading-relaxed text-state-unavailable" data-subagent-tree-truncated="true">
          The scan stopped at its ceiling of {census.sessionsRead.toLocaleString()} sessions. Edges
          into sessions past it are cut, so the counts above describe a prefix of the store and
          not all of it.
        </p>
      ) : null}

      <ul className="flex min-w-0 flex-col" data-subagent-tree-groups={groups.length}>
        {groups.map((group) => (
          <li
            key={`${group.top.provider}:${group.top.session_id}`}
            className="border-b border-edge-subtle py-1 last:border-b-0"
            data-subagent-tree-group={group.nodes.length}
          >
            <ul className="flex min-w-0 flex-col gap-0.5">
              {group.nodes.map((node) => {
                const elapsed = subagentElapsedSeconds(node);
                return (
                  <li
                    key={`${node.provider}:${node.session_id}`}
                    className="flex min-w-0 flex-col"
                    style={{ paddingLeft: `${node.depth * INDENT_REM}rem` }}
                    data-subagent-node={node.session_id}
                    data-subagent-depth={node.depth}
                  >
                    <span className="flex min-w-0 items-baseline gap-1.5">
                      <span className="truncate text-2xs text-text-primary" title={node.session_id}>
                        {subagentLabel(node)}
                      </span>
                      <span className="shrink-0 text-3xs text-text-muted">
                        {node.provider}
                        {node.descendants > 0
                          ? ` · ${node.descendants.toLocaleString()} below`
                          : ''}
                        {elapsed != null ? ` · ${elapsed.toLocaleString()}s` : ' · span unrecorded'}
                      </span>
                    </span>
                    {node.parent_tool_use_id != null && node.link === 'linked' ? (
                      <span className="truncate text-3xs text-text-muted">
                        delegated by tool call {node.parent_tool_use_id}
                      </span>
                    ) : null}
                    <LinkNote node={node} />
                  </li>
                );
              })}
            </ul>
          </li>
        ))}
      </ul>
    </div>
  );
}
