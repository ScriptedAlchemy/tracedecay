import { useCallback, useRef } from 'react';
import { useSearchParams } from 'react-router';
import { cn } from '../../ui/cn.ts';

/**
 * The camera over the Knowledge workspace.
 *
 * Four positions over one store. `facts` is the explorer split this workspace
 * has always been — search, list, fact inspector. The other three read the
 * holographic-memory routes the daemon has been mounting unconsumed:
 * `geometry` is the phase projection and the pairwise similarity it implies,
 * `curation` is what the curator has done and is configured to do, and `oplog`
 * is the store's own append-only record of what changed.
 *
 * They are camera positions rather than separate pages for the same reason
 * Work's are: the selected fact lives in the address, not in a view, so moving
 * the camera keeps it, and a link carrying both parameters reopens the exact
 * position. Both parameters are replaced rather than pushed — panning across
 * four positions is not four places to go back to.
 *
 * The tabs are the ARIA tabs pattern with a roving tabindex: one stop in the
 * page's tab order, arrows between positions, Home and End to the ends.
 * Activation follows focus, which the pattern permits when switching is
 * immediate — and it is: each view owns its own reads, and a view whose read
 * has not landed states that rather than blocking the switch.
 */

export type KnowledgeViewKind = 'facts' | 'geometry' | 'curation' | 'oplog';

export const KNOWLEDGE_VIEWS: readonly KnowledgeViewKind[] = [
  'facts',
  'geometry',
  'curation',
  'oplog',
];

/** The query parameter that positions the camera, so a view survives a reload
 * and can be linked to. */
export const KNOWLEDGE_VIEW_PARAM = 'view';

export function knowledgeViewLabel(kind: KnowledgeViewKind): string {
  switch (kind) {
    case 'facts':
      return 'Facts';
    case 'geometry':
      return 'Geometry';
    case 'curation':
      return 'Curation';
    case 'oplog':
      return 'Oplog';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** What the view reads, printed under the camera so a reader knows what changed
 * when it moves. */
export function knowledgeViewNote(kind: KnowledgeViewKind): string {
  switch (kind) {
    case 'facts':
      return 'facts ranked by trust, with the feedback audit behind each one';
    case 'geometry':
      return 'the phase projection and the pairwise similarity computed from it';
    case 'curation':
      return 'what the curator has done, what its backend runs recorded, and what it is configured to do';
    case 'oplog':
      return 'the store’s append-only record of memory operations';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

function asView(value: string | null): KnowledgeViewKind {
  switch (value) {
    case 'geometry':
    case 'curation':
    case 'oplog':
      return value;
    // An unreadable or absent parameter opens the facts explorer: the one view
    // whose every reading is contracted rather than computed, so it cannot
    // mislead a reader who did not choose it.
    default:
      return 'facts';
  }
}

export function useKnowledgeView(): [KnowledgeViewKind, (kind: KnowledgeViewKind) => void] {
  const [params, setParams] = useSearchParams();
  const active = asView(params.get(KNOWLEDGE_VIEW_PARAM));
  const select = useCallback(
    (kind: KnowledgeViewKind) => {
      const next = new URLSearchParams(params);
      if (kind === 'facts') next.delete(KNOWLEDGE_VIEW_PARAM);
      else next.set(KNOWLEDGE_VIEW_PARAM, kind);
      setParams(next, { replace: true });
    },
    [params, setParams],
  );
  return [active, select];
}

export function knowledgeTabId(kind: KnowledgeViewKind): string {
  return `knowledge-view-${kind}`;
}

/**
 * The region the tabs control, named once so the two halves of the pattern
 * cannot drift apart. Whoever renders this switcher owes the page an element
 * carrying this id for as long as the switcher is on screen: `aria-controls`
 * naming an element that was never drawn is an invalid reference, which is what
 * the accessibility gate reads it as.
 */
export const KNOWLEDGE_PANEL_ID = 'knowledge-view-panel';

export function KnowledgeViewSwitcher({
  active,
  onSelect,
}: {
  active: KnowledgeViewKind;
  onSelect: (kind: KnowledgeViewKind) => void;
}) {
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);

  const move = (from: number, delta: number) => {
    const count = KNOWLEDGE_VIEWS.length;
    const to = (from + delta + count) % count;
    const kind = KNOWLEDGE_VIEWS[to];
    if (kind === undefined) return;
    onSelect(kind);
    tabs.current[to]?.focus();
  };

  const jump = (to: number) => {
    const kind = KNOWLEDGE_VIEWS[to];
    if (kind === undefined) return;
    onSelect(kind);
    tabs.current[to]?.focus();
  };

  return (
    <div
      role="tablist"
      aria-label="Knowledge view"
      aria-orientation="horizontal"
      className="flex min-w-0 flex-wrap items-center gap-1 border border-edge-subtle bg-surface-1 p-1"
      data-knowledge-view={active}
    >
      {KNOWLEDGE_VIEWS.map((kind, position) => {
        const selected = kind === active;
        return (
          <button
            key={kind}
            ref={(node) => {
              tabs.current[position] = node;
            }}
            id={knowledgeTabId(kind)}
            type="button"
            role="tab"
            aria-selected={selected}
            aria-controls={KNOWLEDGE_PANEL_ID}
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
                  jump(KNOWLEDGE_VIEWS.length - 1);
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
            {/* The active view is marked as well as tinted: a camera position
              * must be readable without colour. */}
            <span
              aria-hidden
              className={cn('h-3 w-px shrink-0', selected ? 'bg-accent' : 'bg-edge-strong')}
            />
            {knowledgeViewLabel(kind)}
          </button>
        );
      })}
    </div>
  );
}
