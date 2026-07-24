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

const MAIN = [
  { path: 'brain', label: 'Brain' },
  { path: 'explorer', label: 'Explorer' },
  { path: 'loom', label: 'Loom' },
  { path: 'sessions', label: 'Sessions' },
  { path: 'agents', label: 'Agents' },
  { path: 'code', label: 'Code' },
  { path: 'knowledge', label: 'Knowledge' },
  { path: 'delivery', label: 'Delivery' },
  { path: 'automations', label: 'Automations' },
  { path: 'observatory', label: 'Observatory' },
  { path: 'costs', label: 'Costs' },
];

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
      className={({ isActive }) =>
        cn(
          'group flex h-9 items-center gap-2.5 rounded-[var(--radius-standard)] px-2.5 text-sm',
          'text-text-secondary transition-colors duration-[var(--dur-state)]',
          'hover:bg-surface-2 hover:text-text-primary',
          isActive && 'bg-surface-2 text-text-primary',
        )
      }
    >
      <Icon aria-hidden size={16} strokeWidth={1.5} className="shrink-0" />
      <span className="truncate group-data-[collapsed=true]/rail:hidden">{label}</span>
      {attention ? (
        <span
          className="ml-auto size-1.5 shrink-0 rounded-full bg-state-partial"
          role="status"
          aria-label="Doctor has findings needing attention"
        />
      ) : null}
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
  return result.envelope.payload.entries.some(
    (entry) =>
      entry.finding.state !== 'healthy_complete_coverage' &&
      entry.finding.state !== 'unsupported',
  );
}

/** Navigation only: no status, no badges except the single Doctor attention
 * dot. */
export function NavRail() {
  const attention = useDoctorAttention();
  return (
    <nav
      aria-label="Workspaces"
      className="group/rail flex w-52 shrink-0 flex-col gap-0.5 border-r border-edge-subtle bg-surface-1 p-2 max-md:w-14"
      data-collapsed="false"
    >
      <div className="mb-2 flex h-9 items-center gap-2 px-2.5">
        <span className="size-2 rounded-full bg-accent" aria-hidden />
        <span className="text-sm font-semibold tracking-tight max-md:hidden">TraceDecay</span>
      </div>
      {MAIN.map((w) => (
        <RailLink key={w.path} {...w} attention={w.path === 'observatory' && attention} />
      ))}
      <div className="flex-1" />
      <RailLink path="settings" label="Settings" />
    </nav>
  );
}
