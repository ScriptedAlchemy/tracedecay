import type { LucideIcon } from 'lucide-react';
import {
  Activity,
  BookOpen,
  Bot,
  Boxes,
  Brain,
  Code2,
  Compass,
  GitBranch,
  MessagesSquare,
  Settings,
  Wallet,
  Workflow,
} from 'lucide-react';
import { NavLink } from 'react-router';
import { useQuery } from '@tanstack/react-query';
import { StorageFindingsPayloadSchema } from '../../contracts/wire.ts';
import { fetchEnvelope } from '../../data/query/envelope.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { cn } from '../../ui/cn';
import { CHANNELS, channelNumber } from '../channels.ts';

const ICONS: Record<string, LucideIcon> = {
  brain: Brain,
  explorer: Compass,
  loom: Workflow,
  sessions: MessagesSquare,
  agents: Bot,
  code: Code2,
  knowledge: BookOpen,
  delivery: GitBranch,
  automations: Boxes,
  observatory: Activity,
  costs: Wallet,
  settings: Settings,
};

const MAIN = CHANNELS.filter((channel) => channel.path !== 'settings');

/** A channel selector, not a menu: numbered, letterspaced, hairline-divided.
 * The active channel is marked by a solid signal bar in the gutter — colour
 * used as position, not decoration. */
function RailLink({
  path,
  label,
  attention,
}: {
  path: string;
  label: string;
  attention?: boolean;
}) {
  const Icon = ICONS[path] ?? Boxes;
  return (
    <NavLink
      to={`/${path}`}
      aria-label={label}
      className={({ isActive }) =>
        cn(
          'group relative flex h-8 items-center gap-2.5 border-b border-edge-subtle pl-3.5 pr-2',
          'text-text-secondary transition-colors duration-[var(--dur-state)]',
          'hover:bg-surface-2 hover:text-text-primary max-md:justify-center max-md:px-0',
          isActive && 'bg-surface-2 text-text-primary',
        )
      }
    >
      {({ isActive }) => (
        <>
          <span
            aria-hidden
            className={cn(
              'absolute inset-y-0 left-0 w-[3px]',
              isActive ? 'bg-accent' : 'bg-transparent',
            )}
          />
          <span
            aria-hidden
            className={cn(
              'td-value w-4 shrink-0 text-3xs max-md:hidden',
              isActive ? 'text-accent' : 'text-text-muted',
            )}
          >
            {channelNumber(path)}
          </span>
          <Icon aria-hidden size={13} strokeWidth={1.75} className="shrink-0" />
          <span className="truncate text-3xs font-medium uppercase tracking-[0.14em] max-md:hidden">
            {label}
          </span>
          {attention ? (
            <span
              className="ml-auto size-1.5 shrink-0 bg-state-partial max-md:absolute max-md:right-1 max-md:top-1 max-md:ml-0"
              role="status"
              aria-label="Doctor has findings needing attention"
            />
          ) : null}
        </>
      )}
    </NavLink>
  );
}

/** The single Doctor attention dot (plan 11a): lit only when the findings
 * report carries a non-healthy finding; never a count, never another badge. */
function useDoctorAttention(): boolean {
  const scope = useScope((s) => s.scope);
  const findings = useQuery({
    queryKey: ['storage', 'findings', scopeKey(scope)],
    queryFn: () =>
      fetchEnvelope(scopedUrl(scope, '/api/storage/findings'), StorageFindingsPayloadSchema),
    refetchInterval: 60_000,
  });
  const result = findings.data;
  if (!result || result.outcome === 'transport') return false;
  return result.envelope.payload.kinds.some(
    (kind) => kind.state !== 'healthy_complete_coverage' && kind.state !== 'unsupported',
  );
}

/** Navigation only: no status, no badges except the single Doctor attention
 * dot. */
export function NavRail() {
  const attention = useDoctorAttention();
  return (
    <nav
      aria-label="Workspaces"
      className="group/rail relative flex w-48 shrink-0 flex-col border-r border-edge-subtle bg-surface-1 max-md:w-12"
      data-collapsed="false"
    >
      <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-3 max-md:justify-center max-md:px-0">
        <span aria-hidden className="relative size-3 shrink-0 border border-accent">
          <span className="absolute inset-[3px] bg-accent" />
        </span>
        <span className="min-w-0 max-md:hidden">
          <span className="block truncate text-2xs font-semibold uppercase tracking-[0.2em] text-text-primary">
            TraceDecay
          </span>
        </span>
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {MAIN.map((channel) => (
          <RailLink
            key={channel.path}
            path={channel.path}
            label={channel.label}
            attention={channel.path === 'observatory' && attention}
          />
        ))}
      </div>
      <div className="shrink-0 border-t border-edge-subtle">
        <RailLink path="settings" label="Settings" />
      </div>
    </nav>
  );
}
