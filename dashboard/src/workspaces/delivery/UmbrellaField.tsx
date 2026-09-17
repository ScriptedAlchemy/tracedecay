import { useMemo, type KeyboardEvent } from 'react';
import type {
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
} from '../../contracts/generated.ts';
import { Corners } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { gradeDash, gradeLabel } from './evidence.ts';
import type { UmbrellaProjection } from './umbrella.ts';
import { arcPath, layoutField } from './umbrellaLayout.ts';

/**
 * The Delivery field at outcome zoom — projects as hubs, admitted pull
 * requests in orbit, umbrella outcomes at the centroid of the PRs they
 * correlate, each edge stroked in its grade's dash grammar.
 *
 * The SVG is projected light on the night-glass aperture; it is never the only
 * place a name, count, state or destination can be read. Every node is also a
 * DOM row in the inbox list, the umbrella ledger or the exact table, and the
 * same URL selection drives all of them.
 */
export function UmbrellaField({
  inbox,
  rows,
  projection,
  focusUmbrellaId,
  selectedRowId,
  selectedUmbrellaId,
  onSelectRow,
  onSelectUmbrella,
  className,
}: {
  inbox: DeliveryInboxV1;
  rows: readonly DeliveryInboxPullRequestV1[];
  projection: UmbrellaProjection;
  focusUmbrellaId: string | null;
  selectedRowId: string | null;
  selectedUmbrellaId: string | null;
  onSelectRow: (row: DeliveryInboxPullRequestV1) => void;
  onSelectUmbrella: (umbrellaId: string) => void;
  className?: string;
}) {
  const layout = useMemo(
    () => layoutField(inbox, rows, projection, focusUmbrellaId),
    [focusUmbrellaId, inbox, projection, rows],
  );
  const center = { x: layout.width / 2, y: layout.height / 2 };
  const dimmed = (umbrellaId: string | null, rowId: string | null): boolean => {
    if (selectedUmbrellaId !== null && umbrellaId !== null) return umbrellaId !== selectedUmbrellaId;
    if (selectedUmbrellaId !== null && rowId !== null) {
      const umbrella = projection.umbrellas.find((candidate) => candidate.id === selectedUmbrellaId);
      return umbrella === undefined ? false : !umbrella.members.some((member) => member.id === rowId);
    }
    return false;
  };
  const activate = (event: KeyboardEvent<SVGGElement>, run: () => void) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      run();
    }
  };

  return (
    <div
      className={cn('td-optic td-grain relative min-h-64 flex-1 overflow-hidden', className)}
      data-field="delivery-outcome"
    >
      <Corners tone="signal" />
      <svg
        viewBox={`0 0 ${layout.width} ${layout.height}`}
        preserveAspectRatio="xMidYMid meet"
        className="relative z-[1] block h-full w-full"
        role="group"
        aria-label="Delivery outcome field"
      >
        <defs>
          <pattern id="delivery-graticule" width="32" height="32" patternUnits="userSpaceOnUse">
            <path d="M 32 0 L 0 0 0 32" fill="none" stroke="var(--raw-grid-minor)" strokeWidth="0.5" />
          </pattern>
          <radialGradient id="delivery-halo">
            <stop offset="0%" stopColor="var(--raw-graph-alert)" stopOpacity="0.28" />
            <stop offset="100%" stopColor="var(--raw-graph-alert)" stopOpacity="0" />
          </radialGradient>
          <radialGradient id="delivery-node-glow">
            <stop offset="0%" stopColor="var(--raw-graph-accent)" stopOpacity="0.55" />
            <stop offset="100%" stopColor="var(--raw-graph-accent)" stopOpacity="0" />
          </radialGradient>
        </defs>
        <rect width={layout.width} height={layout.height} fill="url(#delivery-graticule)" aria-hidden />

        {/* Umbrella edges: root → member, stroked by grade. */}
        <g aria-hidden>
          {layout.edges.map((edge) => (
            <path
              key={edge.id}
              d={arcPath(edge.from, edge.to, center)}
              fill="none"
              stroke="var(--raw-graph-alert)"
              strokeWidth={dimmed(edge.umbrellaId, null) ? 0.6 : 1.1}
              strokeOpacity={dimmed(edge.umbrellaId, null) ? 0.25 : 0.7}
              strokeDasharray={gradeDash(edge.grade)}
            />
          ))}
        </g>

        {/* Project hubs. */}
        {layout.hubs.map((hub) => (
          <g key={hub.projectId} aria-hidden>
            <circle
              cx={hub.x}
              cy={hub.y}
              r={hub.radius}
              fill="var(--raw-graph-substrate)"
              stroke="var(--raw-graph-text)"
              strokeOpacity="0.55"
              strokeWidth="1"
            />
            <circle
              cx={hub.x}
              cy={hub.y}
              r={hub.radius + 4}
              fill="none"
              stroke="var(--raw-graph-edge)"
              strokeOpacity="0.5"
              strokeDasharray="1 3"
            />
            <text
              x={hub.x}
              y={hub.y + hub.radius + 14}
              textAnchor="middle"
              fontSize="10"
              fontFamily="var(--font-mono)"
              fill="var(--raw-graph-text)"
              letterSpacing="0.08em"
            >
              {hub.label.toUpperCase()} · {hub.rows}
            </text>
          </g>
        ))}

        {/* Pull request nodes. */}
        {layout.nodes.map((node) => {
          const selected = node.id === selectedRowId;
          const dim = dimmed(null, node.id);
          const title = node.row.pull_request.identity?.title ?? node.row.pull_request.label;
          return (
            <g
              key={node.id}
              role="button"
              tabIndex={0}
              aria-label={`Pull request #${node.row.pull_request.pull_request_id} · ${title} · ${node.active} active attention`}
              aria-pressed={selected}
              className="cursor-pointer outline-none"
              opacity={dim ? 0.3 : 1}
              onClick={() => onSelectRow(node.row)}
              onKeyDown={(event) => activate(event, () => onSelectRow(node.row))}
            >
              {selected ? (
                <circle cx={node.x} cy={node.y} r={node.radius + 14} fill="url(#delivery-node-glow)" />
              ) : null}
              <circle
                cx={node.x}
                cy={node.y}
                r={node.radius}
                fill={selected ? 'var(--raw-graph-accent)' : 'var(--raw-graph-substrate)'}
                stroke="var(--raw-graph-accent)"
                strokeWidth={selected ? 2 : 1.2}
              />
              {node.active > 0 ? (
                <circle
                  cx={node.x + node.radius}
                  cy={node.y - node.radius}
                  r={2.5}
                  fill="var(--raw-graph-alert)"
                />
              ) : null}
              {node.row.state !== 'current' ? (
                <circle
                  cx={node.x}
                  cy={node.y}
                  r={node.radius + 4}
                  fill="none"
                  stroke="var(--raw-graph-text)"
                  strokeOpacity="0.6"
                  strokeDasharray={node.row.state === 'stale' ? '2 2' : '4 3'}
                />
              ) : null}
              <text
                x={node.x + node.radius + 5}
                y={node.y + 3.5}
                fontSize="9.5"
                fontFamily="var(--font-mono)"
                fill="var(--raw-graph-text)"
                opacity={selected ? 1 : 0.8}
              >
                #{node.row.pull_request.pull_request_id}
              </text>
              <title>
                #{node.row.pull_request.pull_request_id} {title} · {node.row.state}
              </title>
            </g>
          );
        })}

        {/* Umbrella roots. */}
        {layout.roots.map((root) => {
          const selected = root.umbrella.id === selectedUmbrellaId;
          const dim = dimmed(root.umbrella.id, null);
          return (
            <g
              key={root.umbrella.id}
              role="button"
              tabIndex={0}
              aria-label={`Umbrella ${root.umbrella.basisLabel} ${root.umbrella.identity} · ${root.umbrella.members.length} pull requests · ${gradeLabel(root.umbrella.grade)}`}
              aria-pressed={selected}
              className="cursor-pointer outline-none"
              opacity={dim ? 0.3 : 1}
              onClick={() => onSelectUmbrella(root.umbrella.id)}
              onKeyDown={(event) => activate(event, () => onSelectUmbrella(root.umbrella.id))}
            >
              <circle cx={root.x} cy={root.y} r={root.radius * 2.2} fill="url(#delivery-halo)" />
              <circle
                cx={root.x}
                cy={root.y}
                r={root.radius}
                fill="var(--raw-graph-substrate)"
                fillOpacity="0.85"
                stroke="var(--raw-graph-alert)"
                strokeWidth={selected ? 2 : 1.2}
                strokeDasharray={gradeDash(root.umbrella.grade)}
              />
              <circle cx={root.x} cy={root.y} r={2.5} fill="var(--raw-graph-alert)" />
              {focusUmbrellaId === root.umbrella.id ? (
                <>
                  <text
                    x={root.x}
                    y={root.y - 6}
                    textAnchor="middle"
                    fontSize="10"
                    fontFamily="var(--font-mono)"
                    fill="var(--raw-graph-alert)"
                    letterSpacing="0.1em"
                  >
                    {root.umbrella.basisLabel.toUpperCase()}
                  </text>
                  <text
                    x={root.x}
                    y={root.y + 10}
                    textAnchor="middle"
                    fontSize="9"
                    fontFamily="var(--font-mono)"
                    fill="var(--raw-graph-text)"
                  >
                    {root.umbrella.members.length} PRs · {root.umbrella.projectIds.length} projects
                  </text>
                </>
              ) : null}
              <title>
                {root.umbrella.basisLabel} {root.umbrella.identity} · {gradeLabel(root.umbrella.grade)}
              </title>
            </g>
          );
        })}
      </svg>
      <p className="pointer-events-none absolute inset-x-0 bottom-0 z-[1] px-3 py-1.5 font-mono text-3xs tracking-[0.08em] text-text-muted">
        each node is an admitted PR · umbrella edges are stroked by grade · exact rows in the list and table
      </p>
    </div>
  );
}
