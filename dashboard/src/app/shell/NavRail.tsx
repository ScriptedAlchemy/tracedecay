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
  ListTodo,
  MessagesSquare,
  Settings,
  Wallet,
  Waypoints,
  Workflow,
} from 'lucide-react';
import { NavLink } from 'react-router';
import type { StorageFindingKindStatusV1 } from '../../contracts/generated.ts';
import { useStorageFindings } from '../../data/query/storageFindings.ts';
import {
  scopedWorkspacePath,
  useScope,
  type DashboardScope,
} from '../../data/scope/store.ts';
import { cn } from '../../ui/cn';
import { CHANNELS, channelNumber } from '../channels.ts';

/** 14-16px monoline glyphs (DESIGN-SYSTEM.md "Brand"). An anatomical Brain
 * icon identifies channel 01; it is never the product logo. */
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
  work: ListTodo,
  // `Workflow` is Loom's icon; the Workflows workspace uses the DAG waypoints
  // mark so the two channels stay distinguishable at a glance.
  workflows: Waypoints,
};

/**
 * What the app-wide Doctor dot is allowed to say.
 *
 * Three states, not two. `unknown` exists because a storage-findings read that
 * never resolved is not a clean bill of health, and rendering it as one turns
 * a broken health check into an all-clear on the app's most global indicator.
 */
type DoctorHealth = 'healthy' | 'attention' | 'unknown';

/**
 * Presentation on the shared evidence axis: the PATTERN says what kind of
 * evidence this is (solid = measured, dashed = none), the token says what the
 * reading means, and the label says both in words. `unknown` is therefore
 * distinguishable from `healthy` without seeing colour at all.
 */
const DOCTOR_HEALTH: Record<
  DoctorHealth,
  { pattern: string; ink: string; label: string }
> = {
  healthy: {
    pattern: 'var(--ev-measured)',
    ink: 'text-state-ready',
    label: 'Doctor storage findings: measured healthy',
  },
  attention: {
    pattern: 'var(--ev-measured)',
    ink: 'text-state-partial',
    label: 'Doctor storage findings: measured findings need attention',
  },
  unknown: {
    pattern: 'var(--ev-unknown)',
    ink: 'text-state-unknown',
    label: 'Doctor storage findings: could not be read, health unknown',
  },
};

/**
 * How one storage-finding producer reads for the global dot.
 *
 * A producer only counts as health when it actually looked (`real`) and found
 * nothing. Anything it did observe is a finding. Everything else, a source the
 * canonical report only partially observed, or a producer unsupported on this
 * store established nothing either way, and reporting "no evidence" as a clean
 * bill of health is the whole defect this dot exists to avoid.
 */
function kindHealth(status: StorageFindingKindStatusV1): DoctorHealth {
  if (status.observed_entries > 0) return 'attention';
  switch (status.state) {
    case 'real':
      return 'healthy';
    case 'partial':
    case 'unsupported':
      return 'unknown';
    default: {
      const unhandled: never = status.state;
      return unhandled;
    }
  }
}

/**
 * One channel on the rail (NAVIGATION.md "Behavior").
 *
 * Selected: a 3px cyan gutter, the raised face, a cyan channel number and a
 * white label, position marked by colour, not decoration. Hover inspects:
 * the face rises and the text brightens, and nothing about selection, scope
 * or any measured value changes. Keyboard focus is the design system's 2px
 * cyan outline from the base layer; hover never substitutes for it.
 */
function RailLink({
  path,
  label,
  health,
  scope,
}: {
  path: string;
  label: string;
  health?: DoctorHealth;
  scope: DashboardScope;
}) {
  const Icon = ICONS[path] ?? Boxes;
  return (
    <NavLink
      to={scopedWorkspacePath(scope, path)}
      aria-label={label}
      className={({ isActive }) =>
        cn(
          // The row is the target, so the row carries the 44px minimum rather
          // than the glyph growing. Compact, the rail is 48px wide and the
          // link fills it, so the row clears the minimum on both axes.
          'group relative flex min-h-[var(--touch-target-min)] items-center gap-2.5 border-b border-edge-subtle pl-3.5 pr-2',
          'text-text-secondary transition-colors duration-[var(--dur-state)]',
          'hover:bg-surface-2 hover:text-text-primary max-md:justify-center max-md:px-0',
          isActive && 'td-raised text-text-primary',
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
              'td-value w-5 shrink-0 text-2xs max-md:hidden',
              isActive ? 'text-accent' : 'text-text-muted',
            )}
          >
            {channelNumber(path)}
          </span>
          <Icon aria-hidden size={14} strokeWidth={1.5} className="shrink-0" />
          <span className="truncate text-sm">{label}</span>
          {health ? <DoctorDot health={health} /> : null}
        </>
      )}
    </NavLink>
  );
}

/** The single Doctor dot (plan 11a): one mark, never a count, never another
 * badge, but it reports its own reading rather than only its worst one. */
function DoctorDot({ health }: { health: DoctorHealth }) {
  const presentation = DOCTOR_HEALTH[health];
  return (
    <span
      role="status"
      aria-label={presentation.label}
      data-doctor-health={health}
      className={cn(
        'ml-auto size-2 shrink-0 border border-current',
        'max-md:absolute max-md:right-1 max-md:top-1 max-md:ml-0',
        presentation.ink,
      )}
      // The evidence patterns are drawn in `currentColor`, so the ink token
      // above colours the fill and the pattern carries the evidence class.
      style={{ backgroundImage: presentation.pattern }}
    />
  );
}

/**
 * The global Doctor reading.
 *
 * Every transport outcome used to return `false` here, so "the storage-findings
 * read is broken" and "the system is verified healthy" rendered as the same
 * pixels on the app-wide indicator. A read that failed, has not landed, or came
 * back with nothing to read is `unknown`; only a resolved report whose every
 * producer looked and found nothing is `healthy`.
 *
 * Read through {@link useStorageFindings}, which owns the key, the route, the
 * generated contract, and the poll. Observatory reads the same entry; when this
 * file named its own period the two disagreed, and the shared entry took the
 * shorter one regardless of what was written here.
 */
function useDoctorHealth(): DoctorHealth {
  const findings = useStorageFindings();
  const result = findings.data;
  if (!result || result.outcome === 'transport') return 'unknown';
  const statuses = result.envelope.payload.storage_kind_statuses;
  // A report naming no producers has established nothing about this store.
  if (statuses.length === 0) return 'unknown';
  const readings = statuses.map(kindHealth);
  if (readings.includes('attention')) return 'attention';
  if (readings.includes('unknown')) return 'unknown';
  return 'healthy';
}

/**
 * The brand block (NAVIGATION.md "Persistent regions" 1): the trace-tail
 * glyph and the wordmark at the top of the rail.
 *
 * Identity only. It is not a link, Brain is reached through channel 01, and
 * the shipping shell exposes no logo action, and it never visualizes
 * activity, health, connectivity, feed or work. The glyph is static CSS with
 * no state input by construction; nothing here can be wired to a reading
 * without becoming a different component. Sized to the register beside it so
 * the hairline under both runs unbroken across the shell.
 */
function BrandBlock() {
  return (
    <div
      data-brand
      className="flex min-h-[var(--shell-register)] shrink-0 flex-col justify-center gap-2 border-b border-edge-frame px-3.5 max-md:items-center max-md:px-0"
    >
      <span className="td-wordmark max-md:hidden">TraceDecay</span>
      <span aria-hidden className="td-trace-tail">
        <i />
        <i />
        <i />
        <i />
      </span>
    </div>
  );
}

/**
 * The navigation rail (NAVIGATION.md "Persistent regions" 2): all fourteen
 * numbered workspaces in their fixed order, 192px expanded or 48px compact.
 *
 * Navigation only: no status, no badges except the single Doctor health dot
 * on Observatory, which is where Doctor lives. One flat register, the rail
 * used to file channels under group dividers and pin Settings to its foot,
 * which drew channel 12 below channel 14; the canonical rail draws them in
 * numeric order and nowhere else.
 */
export function NavRail() {
  const health = useDoctorHealth();
  const scope = useScope((state) => state.scope);
  return (
    <nav
      aria-label="Workspaces"
      className="group/rail relative flex w-[var(--shell-rail)] shrink-0 flex-col border-r border-edge-frame bg-surface-1 max-md:w-[var(--shell-rail-compact)]"
      data-collapsed="false"
    >
      <BrandBlock />
      <div className="min-h-0 flex-1 overflow-auto">
        {CHANNELS.map((channel) => (
          <RailLink
            key={channel.path}
            path={channel.path}
            label={channel.label}
            health={channel.path === 'observatory' ? health : undefined}
            scope={scope}
          />
        ))}
      </div>
    </nav>
  );
}
