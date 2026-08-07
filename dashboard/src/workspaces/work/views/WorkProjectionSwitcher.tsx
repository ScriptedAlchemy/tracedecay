import { useCallback, useRef } from 'react';
import { useSearchParams } from 'react-router';
import { cn } from '../../../ui/cn.ts';

/**
 * The camera over the Work workspace.
 *
 * Plan 11's mandate, restated by 11c: one canonical selection, many
 * synchronized projections. The switcher moves the camera and never the
 * selection — it writes `?view`, and the selected task stays in `?task` where
 * `useSelectedTask` owns it. Switching projection therefore keeps the task you
 * were reading, and a link carrying both parameters reopens the exact position.
 *
 * Both parameters are replaced rather than pushed. Panning a camera across
 * five positions is not five places to go back to.
 *
 * The tabs implement the ARIA tabs pattern with a roving tabindex: one stop in
 * the page's tab order, arrows move between projections, Home and End jump to
 * the ends. Activation follows focus, which the pattern permits when switching
 * is immediate — every projection is derived from a snapshot already in hand,
 * so there is no fetch to make an arrow key expensive.
 */

export type WorkProjectionKind = 'board' | 'dag' | 'timeline' | 'causal' | 'workload';

export const WORK_PROJECTIONS: readonly WorkProjectionKind[] = [
  'board',
  'dag',
  'timeline',
  'causal',
  'workload',
];

/** The query parameter that positions the camera, so a projection survives a
 * reload and can be linked to. */
export const VIEW_PARAM = 'view';

export function projectionLabel(kind: WorkProjectionKind): string {
  switch (kind) {
    case 'board':
      return 'Board';
    case 'dag':
      return 'DAG';
    case 'timeline':
      return 'Timeline';
    case 'causal':
      return 'Causal';
    case 'workload':
      return 'Workload';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** What the projection measures, printed under the camera so a reader knows
 * what changed when it moves. */
export function projectionNote(kind: WorkProjectionKind): string {
  switch (kind) {
    case 'board':
      return 'tasks grouped by the furthest gate each has passed';
    case 'dag':
      return 'declared dependencies, layered by longest path';
    case 'timeline':
      return 'runs woven across the tasks they attached evidence to';
    case 'causal':
      return 'declared dependencies read against the evidence at both ends';
    case 'workload':
      return 'runs as regions, sized by the tasks they touched';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

function asProjection(value: string | null): WorkProjectionKind {
  switch (value) {
    case 'dag':
    case 'timeline':
    case 'causal':
    case 'workload':
      return value;
    // An unreadable or absent parameter opens the board. The board is the
    // projection whose every channel this build can measure, so it is the one
    // position that cannot mislead a reader who did not choose it.
    default:
      return 'board';
  }
}

export function useWorkProjection(): [WorkProjectionKind, (kind: WorkProjectionKind) => void] {
  const [params, setParams] = useSearchParams();
  const active = asProjection(params.get(VIEW_PARAM));
  const select = useCallback(
    (kind: WorkProjectionKind) => {
      const next = new URLSearchParams(params);
      if (kind === 'board') next.delete(VIEW_PARAM);
      else next.set(VIEW_PARAM, kind);
      setParams(next, { replace: true });
    },
    [params, setParams],
  );
  return [active, select];
}

export function tabId(kind: WorkProjectionKind): string {
  return `work-projection-${kind}`;
}

/**
 * The region the tabs control, named once so the two halves of the pattern
 * cannot drift apart.
 *
 * Whoever renders this switcher owes the page an element carrying this id, for
 * as long as the switcher is on screen. `aria-controls` is a reference, and a
 * reference to an element that was never drawn is not a weaker control — it is
 * an invalid one, which is what the accessibility gate reads it as.
 */
export const PROJECTION_PANEL_ID = 'work-projection-panel';

export function WorkProjectionSwitcher({
  active,
  onSelect,
}: {
  active: WorkProjectionKind;
  onSelect: (kind: WorkProjectionKind) => void;
}) {
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);

  const move = (from: number, delta: number) => {
    const count = WORK_PROJECTIONS.length;
    const to = (from + delta + count) % count;
    const kind = WORK_PROJECTIONS[to];
    if (kind === undefined) return;
    onSelect(kind);
    tabs.current[to]?.focus();
  };

  const jump = (to: number) => {
    const kind = WORK_PROJECTIONS[to];
    if (kind === undefined) return;
    onSelect(kind);
    tabs.current[to]?.focus();
  };

  return (
    <div
      role="tablist"
      aria-label="Work projection"
      aria-orientation="horizontal"
      className="flex min-w-0 flex-wrap items-center gap-1 border border-edge-subtle bg-surface-1 p-1"
      data-work-projection={active}
    >
      {WORK_PROJECTIONS.map((kind, position) => {
        const selected = kind === active;
        return (
          <button
            key={kind}
            ref={(node) => {
              tabs.current[position] = node;
            }}
            id={tabId(kind)}
            type="button"
            role="tab"
            aria-selected={selected}
            aria-controls={PROJECTION_PANEL_ID}
            // Roving tabindex: the tablist is one stop in the page's tab
            // order and the arrows move within it.
            tabIndex={selected ? 0 : -1}
            onClick={() => onSelect(kind)}
            onKeyDown={(event) => {
              switch (event.key) {
                case 'ArrowRight':
                case 'ArrowDown':
                  event.preventDefault();
                  move(position, 1);
                  break;
                case 'ArrowLeft':
                case 'ArrowUp':
                  event.preventDefault();
                  move(position, -1);
                  break;
                case 'Home':
                  event.preventDefault();
                  jump(0);
                  break;
                case 'End':
                  event.preventDefault();
                  jump(WORK_PROJECTIONS.length - 1);
                  break;
                default:
                  break;
              }
            }}
            className={cn(
              // 44px explicitly, not `min-h-11`: this app's root font size is
              // 14px, so a spacing-11 minimum computes to 38.5px and lands
              // under the target size the accessibility gate measures.
              'flex min-h-[44px] items-center gap-2 border px-3 text-2xs',
              'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
              selected
                ? 'border-edge-strong bg-surface-3 text-text-primary'
                : 'border-transparent text-text-secondary hover:bg-surface-2',
            )}
          >
            {/* The active projection is marked as well as tinted: a camera
              * position must be readable without colour. */}
            <span
              aria-hidden
              className={cn('h-3 w-px shrink-0', selected ? 'bg-accent' : 'bg-edge-strong')}
            />
            {projectionLabel(kind)}
          </button>
        );
      })}
    </div>
  );
}
