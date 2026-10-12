import type { LucideIcon } from 'lucide-react';
import { Bot, MessagesSquare, Waypoints } from 'lucide-react';
import { Link } from 'react-router';
import { scopedWorkspacePath, useScope, type DashboardScope } from '../../data/scope/store.ts';
import { sessionLaneId } from '../../viz/temporal/journey.ts';
import { LOOM_PARAMS } from '../loom/loomUrl.ts';
import { writeSessionSelection, type SessionSelection } from './model.ts';

export type SessionWorkspace = 'sessions' | 'loom' | 'agents';

const PIVOTS: Record<SessionWorkspace, { label: string; icon: LucideIcon }> = {
  sessions: { label: 'Open in Sessions', icon: MessagesSquare },
  loom: { label: 'Open in Loom', icon: Waypoints },
  agents: { label: 'Open in Agents', icon: Bot },
};

export function sessionHref(
  target: SessionWorkspace,
  session: SessionSelection,
  scope: DashboardScope,
): string {
  const [pathname, search = ''] = scopedWorkspacePath(scope, target).split('?');
  const params = new URLSearchParams(search);
  if (target !== 'loom') return `${pathname}?${writeSessionSelection(params, session).toString()}`;
  params.set(
    LOOM_PARAMS.session,
    session.provider === null
      ? session.sessionId
      : sessionLaneId(session.provider, session.sessionId),
  );
  return `${pathname}?${params.toString()}`;
}

export function SessionPivots({
  session,
  from,
}: {
  session: SessionSelection;
  from: SessionWorkspace;
}) {
  const scope = useScope((state) => state.scope);
  const targets = (Object.keys(PIVOTS) as SessionWorkspace[]).filter(
    (target) => target !== from && (target !== 'loom' || session.provider !== null),
  );
  return (
    <nav aria-label="Open this session in" className="flex flex-wrap gap-2">
      {targets.map((target) => {
        const { label, icon: Icon } = PIVOTS[target];
        return (
          <Link
            key={target}
            to={sessionHref(target, session, scope)}
            className="td-hit group self-start"
            data-pivot={target}
          >
            <span className="inline-flex items-center gap-1 border border-edge-subtle bg-surface-2 px-2 py-1 text-sm text-text-secondary group-hover:text-text-primary">
              <Icon aria-hidden size={11} />
              {label}
            </span>
          </Link>
        );
      })}
    </nav>
  );
}
