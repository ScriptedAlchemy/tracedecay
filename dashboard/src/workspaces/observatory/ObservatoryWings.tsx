import { useCallback, useRef } from 'react';
import { useSearchParams } from 'react-router';
import { cn } from '../../ui/cn.ts';

/**
 * The contextual wings over the Observatory.
 *
 * The workspace used to render every read model it owns in one unbounded
 * stack — fifteen heavyweight sections, tens of thousands of pixels tall at
 * tablet widths — which is a wall, not an instrument. A wing is the active
 * mode's panel set, expanded on demand: Diagnosis is the default because the
 * Doctor evidence (canonical findings, storage budgets, orphan census,
 * incident debris, store telemetry) is what this channel exists to report;
 * the other wings unfold the accounting, performance, and hint registers
 * without burying the diagnosis under them.
 *
 * Wings are camera positions, not pages: the active wing lives in the address
 * (`?wing=`), replaced rather than pushed, so a link reopens the exact
 * position and panning across four positions is not four places to go back
 * to. The tabs are the ARIA tabs pattern with a roving tabindex, identical to
 * the Knowledge camera.
 */

export type ObservatoryWingKind = 'diagnosis' | 'adoption' | 'performance' | 'signals';

export const OBSERVATORY_WINGS: readonly ObservatoryWingKind[] = [
  'diagnosis',
  'adoption',
  'performance',
  'signals',
];

/** The query parameter that positions the camera, so a wing survives a reload
 * and can be linked to. */
export const OBSERVATORY_WING_PARAM = 'wing';

export function observatoryWingLabel(kind: ObservatoryWingKind): string {
  switch (kind) {
    case 'diagnosis':
      return 'Diagnosis';
    case 'adoption':
      return 'Adoption';
    case 'performance':
      return 'Performance';
    case 'signals':
      return 'Signals';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** What the wing reads, printed beside the switcher so a reader knows what
 * changed when the camera moves. */
export function observatoryWingNote(kind: ObservatoryWingKind): string {
  switch (kind) {
    case 'diagnosis':
      return 'canonical Doctor evidence, storage findings, store telemetry, and the code-index pipeline';
    case 'adoption':
      return 'canonical observations, adoption coverage and outcomes, retrieval quality, and rejected arguments';
    case 'performance':
      return 'performance budgets, comparisons, and execution-topology measurements';
    case 'signals':
      return 'hook hint outcomes and the analytics run controls';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

function asWing(value: string | null): ObservatoryWingKind {
  switch (value) {
    case 'adoption':
    case 'performance':
    case 'signals':
      return value;
    // An unreadable or absent parameter opens the diagnosis wing: the Doctor
    // evidence is the channel's reason to exist, so it is what an unqualified
    // link lands on.
    default:
      return 'diagnosis';
  }
}

export function useObservatoryWing(): [
  ObservatoryWingKind,
  (kind: ObservatoryWingKind) => void,
] {
  const [params, setParams] = useSearchParams();
  const active = asWing(params.get(OBSERVATORY_WING_PARAM));
  const select = useCallback(
    (kind: ObservatoryWingKind) => {
      const next = new URLSearchParams(params);
      if (kind === 'diagnosis') next.delete(OBSERVATORY_WING_PARAM);
      else next.set(OBSERVATORY_WING_PARAM, kind);
      setParams(next, { replace: true });
    },
    [params, setParams],
  );
  return [active, select];
}

export function observatoryWingTabId(kind: ObservatoryWingKind): string {
  return `observatory-wing-${kind}`;
}

/** The region the tabs control, named once so the two halves of the pattern
 * cannot drift apart. */
export const OBSERVATORY_WING_PANEL_ID = 'observatory-wing-panel';

export function ObservatoryWingSwitcher({
  active,
  onSelect,
}: {
  active: ObservatoryWingKind;
  onSelect: (kind: ObservatoryWingKind) => void;
}) {
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);

  const move = (from: number, delta: number) => {
    const count = OBSERVATORY_WINGS.length;
    const to = (from + delta + count) % count;
    const kind = OBSERVATORY_WINGS[to];
    if (kind === undefined) return;
    onSelect(kind);
    tabs.current[to]?.focus();
  };

  const jump = (to: number) => {
    const kind = OBSERVATORY_WINGS[to];
    if (kind === undefined) return;
    onSelect(kind);
    tabs.current[to]?.focus();
  };

  return (
    <div
      role="tablist"
      aria-label="Observatory wing"
      aria-orientation="horizontal"
      className="flex min-w-0 flex-wrap items-center gap-1 border border-edge-subtle bg-surface-1 p-1"
      data-observatory-wing={active}
    >
      {OBSERVATORY_WINGS.map((kind, position) => {
        const selected = kind === active;
        return (
          <button
            key={kind}
            ref={(node) => {
              tabs.current[position] = node;
            }}
            id={observatoryWingTabId(kind)}
            type="button"
            role="tab"
            aria-selected={selected}
            aria-controls={OBSERVATORY_WING_PANEL_ID}
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
                  jump(OBSERVATORY_WINGS.length - 1);
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
            {/* The active wing is marked as well as tinted: a camera position
             * must be readable without colour. */}
            <span
              aria-hidden
              className={cn('h-3 w-px shrink-0', selected ? 'bg-accent' : 'bg-edge-strong')}
            />
            {observatoryWingLabel(kind)}
          </button>
        );
      })}
    </div>
  );
}
