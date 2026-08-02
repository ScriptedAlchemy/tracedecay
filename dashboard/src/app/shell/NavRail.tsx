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
  Workflow,
} from 'lucide-react';
import { NavLink } from 'react-router';
import type { StorageFindingKindStatusV1 } from '../../contracts/generated.ts';
import { useStorageFindings } from '../../data/query/storageFindings.ts';
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
  work: ListTodo,
};

const MAIN = CHANNELS.filter((channel) => channel.path !== 'settings');

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
 * nothing. Anything it did observe is a finding. Everything else — a source the
 * owner never configured, a partial sweep, a producer unsupported on this store
 * — established nothing either way, and reporting "no evidence" as a clean bill
 * of health is the whole defect this dot exists to avoid.
 */
function kindHealth(status: StorageFindingKindStatusV1): DoctorHealth {
  if (status.observed_entries > 0) return 'attention';
  switch (status.state) {
    case 'real':
      return 'healthy';
    case 'partial':
    case 'unset':
    case 'unsupported':
      return 'unknown';
    default: {
      const unhandled: never = status.state;
      return unhandled;
    }
  }
}

/** A channel selector, not a menu: numbered, letterspaced, hairline-divided.
 * The active channel is marked by a solid signal bar in the gutter — colour
 * used as position, not decoration. */
function RailLink({
  path,
  label,
  health,
}: {
  path: string;
  label: string;
  health?: DoctorHealth;
}) {
  const Icon = ICONS[path] ?? Boxes;
  return (
    <NavLink
      to={`/${path}`}
      aria-label={label}
      className={({ isActive }) =>
        cn(
          // A workspace link is the most-used control in the product and it
          // rendered 41x28 — under the minimum on both axes once the rail
          // collapses to icons. The row is the target, so the row carries the
          // minimum rather than the glyph growing.
          'group relative flex min-h-[var(--touch-target-min)] items-center gap-2.5 border-b border-edge-subtle pl-3.5 pr-2',
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
          {health ? <DoctorDot health={health} /> : null}
        </>
      )}
    </NavLink>
  );
}

/** The single Doctor dot (plan 11a): one mark, never a count, never another
 * badge — but it reports its own reading rather than only its worst one. */
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
  const statuses = result.envelope.payload.kind_statuses;
  // A report naming no producers has established nothing about this store.
  if (statuses.length === 0) return 'unknown';
  const readings = statuses.map(kindHealth);
  if (readings.includes('attention')) return 'attention';
  if (readings.includes('unknown')) return 'unknown';
  return 'healthy';
}

/** Navigation only: no status, no badges except the single Doctor health
 * dot. */
export function NavRail() {
  const health = useDoctorHealth();
  return (
    <nav
      aria-label="Workspaces"
      // Collapsed to icons the rail was `w-12` — 42px, so every link in it was
      // 41px wide against a 44px minimum. The collapsed width is the link
      // width, so it comes from the token plus the rail's own hairline.
      className="group/rail relative flex w-48 shrink-0 flex-col border-r border-edge-subtle bg-surface-1 max-md:w-[calc(var(--touch-target-min)+1px)]"
      data-collapsed="false"
    >
      {/* Matches the `ScopeBar` floor beside it, which is sized from the same
        * token so its stretched controls clear the minimum. A floor rather than
        * a height for the same reason: the bar grows under text-only zoom rather
        * than clipping its scope, and a fixed brand block would hold this
        * hairline at 45px while the one beside it moved. */}
      <div className="flex min-h-[calc(var(--touch-target-min)+1px)] shrink-0 items-center gap-2.5 border-b border-edge-subtle px-3 max-md:justify-center max-md:px-0">
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
            health={channel.path === 'observatory' ? health : undefined}
          />
        ))}
      </div>
      <div className="shrink-0 border-t border-edge-subtle">
        <RailLink path="settings" label="Settings" />
      </div>
    </nav>
  );
}
