/**
 * The effective-configuration table: KEY · VALUE · PROVENANCE · ORIGIN · WRITE,
 * one row per served scalar, grouped under sticky section rows.
 *
 * Hover and focus INSPECT a row: the inspector shows its exact evidence and
 * nothing on the row changes. Click, Enter, or Space SELECT it: the review
 * state opens directly under it. Escape closes the review, then clears the
 * inspection. Arrow keys rove between rows so a reader never has to Tab through
 * fifty keys to reach the one they want.
 *
 * Every cell is a literal the read model found or a typed statement about it.
 * The VALUE column always shows the effective value the daemon reported; an
 * edited proposal appears beside it, labelled, and never replaces it until the
 * write authority has applied it and the read has come back.
 */

import {
  Fragment,
  useCallback,
  useEffect,
  useRef,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import type { CodeIndexWorkerStatusV1 } from '../../contracts/generated.ts';
import { cn } from '../../ui/cn';
import { elideStart } from '../../ui/format.ts';
import type { SettingsEditorHandle } from './SettingsEditorController.tsx';
import type { SettingsEditorState } from './settingsEditorMachine.ts';
import type { WritableScopes } from './settingsGates.ts';
import type { ConfigSection } from './settingsModel.ts';
import { SettingsReviewPanel } from './SettingsReviewPanel.tsx';
import {
  bindingFor,
  draftValue,
  fieldEdited,
  writeCapability,
  type EffectiveRow,
  type WriteCapability,
} from './settingsRows.ts';
import {
  KeyText,
  ORIGIN_WORD,
  OriginMark,
  PathText,
  ProvenanceChip,
  ValueCell,
  WriteCell,
  proposalText,
  type RowProvenance,
} from './SettingsValues.tsx';

export interface SectionGroup {
  readonly section: ConfigSection;
  readonly rows: readonly EffectiveRow[];
}

/**
 * Column tracks switch on the TABLE's own width, not the viewport's: at `lg`
 * the table sits between the sections rail and the inspector and may be
 * narrower than it is at `md` with both stacked. The breakpoint is the sum of
 * the minimum tracks, the four column gaps, and the row padding, so a table
 * that shows columns can always fit them; narrower than that each row stacks
 * its cells with the column name printed beside each value.
 */
const COLUMNS =
  '@min-[41rem]:grid-cols-[minmax(9rem,1.3fr)_minmax(8rem,1.2fr)_6.5rem_minmax(9.5rem,1.2fr)_4.5rem]';
const COLUMN_GAP = 'gap-x-2';

/** The machine states in which a frozen review, and its controls, are on screen. */
const REVIEW_BEARING: ReadonlySet<SettingsEditorState['status']> = new Set([
  'reviewing',
  'confirmed',
  'submitting',
  'conflicted',
  'authority_withdrawn',
  'submit_failed',
  'review_superseded',
]);

export function EffectiveConfigTable({
  groups,
  query,
  gates,
  editor,
  workerStatus,
  inspectedKey,
  selectedKey,
  onInspect,
  onSelect,
  scrollRef,
}: {
  groups: readonly SectionGroup[];
  query: string;
  gates: WritableScopes;
  editor: SettingsEditorHandle;
  workerStatus: CodeIndexWorkerStatusV1 | null;
  inspectedKey: string | null;
  selectedKey: string | null;
  onInspect: (key: string | null) => void;
  onSelect: (key: string | null) => void;
  scrollRef: RefObject<HTMLDivElement | null>;
}) {
  const visible = (key: string | null) =>
    key !== null && groups.some((group) => group.rows.some((row) => row.key === key));
  // Roving tabindex: the row focus last rested on (focus sets `inspectedKey`),
  // else the selected row, else the first — so Tab out and Shift+Tab back lands
  // where the reader left, not at the top of the table.
  const firstKey = groups[0]?.rows[0]?.key ?? null;
  const tabbableKey = visible(inspectedKey)
    ? inspectedKey
    : visible(selectedKey)
      ? selectedKey
      : firstKey;
  const editorAvailable = editor.state.status !== 'editor_unavailable';
  // While a write is in flight the selection is pinned: closing the panel would
  // hide the verdict the reader is waiting for.
  const pinned = editor.state.status === 'submitting';

  // Focus returns to the edited row when a frozen review resolves and the
  // control the reader was on — Apply, Retry, Load current values — unmounts
  // with the review stage. Only when focus was actually lost to the document:
  // a keystroke in another scope's input also ends a staged review, and that
  // input must keep its focus.
  const status = editor.state.status;
  const previousStatus = useRef(status);
  useEffect(() => {
    const wasStaged = REVIEW_BEARING.has(previousStatus.current);
    previousStatus.current = status;
    if (!wasStaged || status !== 'editing' || selectedKey === null) return;
    const active = document.activeElement;
    const container = scrollRef.current;
    if (active === null || active === document.body || !container?.contains(active)) {
      focusRow(container, selectedKey);
    }
  }, [status, selectedKey, scrollRef]);

  const onKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      const container = scrollRef.current;
      if (!container) return;
      if (event.key === 'Escape') {
        if (selectedKey !== null) {
          if (pinned) return;
          event.preventDefault();
          onSelect(null);
          focusRow(container, selectedKey);
        } else if (inspectedKey !== null) {
          event.preventDefault();
          onInspect(null);
        }
        return;
      }
      // Rove only from a row; a key pressed inside the review panel's inputs
      // belongs to the input.
      if (!(event.target instanceof HTMLElement) || event.target.getAttribute('role') !== 'row') {
        return;
      }
      const rows = [...container.querySelectorAll<HTMLElement>('[role="row"][data-key]')];
      if (rows.length === 0) return;
      const current = rows.indexOf(event.target);
      const last = rows.length - 1;
      const from = current < 0 ? 0 : current;
      let next: number;
      switch (event.key) {
        case 'ArrowDown':
          next = Math.min(from + 1, last);
          break;
        case 'ArrowUp':
          next = Math.max(from - 1, 0);
          break;
        case 'Home':
          next = 0;
          break;
        case 'End':
          next = last;
          break;
        case 'PageDown':
          next = Math.min(from + 10, last);
          break;
        case 'PageUp':
          next = Math.max(from - 10, 0);
          break;
        default:
          return;
      }
      event.preventDefault();
      rows[next]?.focus();
      rows[next]?.scrollIntoView({ block: 'nearest' });
    },
    [scrollRef, selectedKey, inspectedKey, pinned, onSelect, onInspect],
  );

  return (
    <div
      ref={scrollRef}
      role="grid"
      aria-label="Effective configuration"
      onKeyDown={onKeyDown}
      className="@container relative min-h-[var(--pane-min-height)] min-w-0 flex-1 overflow-auto td-well"
    >
      <div
        role="row"
        className={cn(
          'sticky top-0 z-20 hidden border-b border-edge-subtle bg-surface-1 px-3 @min-[41rem]:grid',
          COLUMNS,
          COLUMN_GAP,
        )}
      >
        {['key', 'value', 'provenance', 'origin', 'write'].map((column) => (
          <span key={column} role="columnheader" className="td-legend py-2">
            {column}
          </span>
        ))}
      </div>
      {groups.map(({ section, rows }) => (
        <div key={section.id} role="rowgroup" data-section={section.id} className="min-w-0">
          <SectionRow section={section} count={rows.length} />
          {rows.map((row) => {
            const capability = writeCapability(row.key, gates, editorAvailable);
            const selected = row.key === selectedKey;
            return (
              <Fragment key={row.key}>
                <ConfigRowLine
                  row={row}
                  query={query}
                  capability={capability}
                  editor={editor}
                  inspected={row.key === inspectedKey}
                  selected={selected}
                  tabbable={row.key === tabbableKey}
                  pinned={pinned}
                  onInspect={onInspect}
                  onSelect={onSelect}
                />
                {selected ? (
                  <div role="row" aria-selected={false}>
                    <div role="gridcell" aria-colspan={5}>
                      <SettingsReviewPanel
                        row={row}
                        capability={capability}
                        editor={editor}
                        workerStatus={workerStatus}
                        query={query}
                        closable={!pinned}
                        onClose={() => {
                          onSelect(null);
                          focusRow(scrollRef.current, row.key);
                        }}
                      />
                    </div>
                  </div>
                ) : null}
              </Fragment>
            );
          })}
        </div>
      ))}
    </div>
  );
}

function focusRow(container: HTMLElement | null, key: string | null): void {
  if (!container || key === null) return;
  const row = [...container.querySelectorAll<HTMLElement>('[role="row"][data-key]')].find(
    (candidate) => candidate.dataset['key'] === key,
  );
  row?.focus();
}

/** The section's own line: origin glyph and word, what the group is, the
 * location the payload names, and how many of its keys the filter shows. */
function SectionRow({ section, count }: { section: ConfigSection; count: number }) {
  return (
    <div
      role="row"
      className="sticky top-0 z-10 flex flex-wrap items-center gap-x-2.5 gap-y-0.5 border-y border-edge-subtle bg-surface-2 px-3 py-1.5 @min-[41rem]:top-7"
    >
      <div
        role="gridcell"
        aria-colspan={5}
        className="flex min-w-0 flex-1 flex-wrap items-center gap-x-2.5 gap-y-0.5"
      >
        <OriginMark origin={section.origin} />
        <h2 className="text-xs font-semibold tracking-tight">{section.title}</h2>
        <span className="td-legend">{ORIGIN_WORD[section.origin]}</span>
        <span className="min-w-0 truncate text-3xs text-text-muted">{section.blurb}</span>
        {section.location ? (
          <span className="td-value min-w-0 truncate text-3xs">
            {section.locationKind === 'path' ? (
              <PathText value={section.location} />
            ) : (
              <span className="text-text-secondary">{section.location}</span>
            )}
          </span>
        ) : null}
        {section.notes.map((note) => (
          <span
            key={note}
            className="border border-edge-subtle px-1.5 py-px text-3xs text-text-secondary"
          >
            {note}
          </span>
        ))}
        <span className="td-value ml-auto shrink-0 text-3xs text-text-muted" data-cell="numeric">
          {count}
        </span>
      </div>
    </div>
  );
}

function ConfigRowLine({
  row,
  query,
  capability,
  editor,
  inspected,
  selected,
  tabbable,
  pinned,
  onInspect,
  onSelect,
}: {
  row: EffectiveRow;
  query: string;
  capability: WriteCapability;
  editor: SettingsEditorHandle;
  inspected: boolean;
  selected: boolean;
  tabbable: boolean;
  pinned: boolean;
  onInspect: (key: string | null) => void;
  onSelect: (key: string | null) => void;
}) {
  const binding = bindingFor(row.key);
  const { state } = editor;
  const proposed =
    binding !== null &&
    state.status !== 'editor_unavailable' &&
    fieldEdited(state.draft, state.authority, binding)
      ? proposalText(draftValue(state.draft, binding))
      : null;
  const provenance: RowProvenance = proposed !== null ? 'edited' : row.row.provenance;
  const toggle = () => {
    if (pinned) return;
    onSelect(selected ? null : row.key);
  };
  return (
    <div
      role="row"
      aria-selected={selected}
      tabIndex={tabbable ? 0 : -1}
      data-key={row.key}
      data-provenance={provenance}
      data-write={capability.kind}
      onClick={toggle}
      onFocus={() => onInspect(row.key)}
      onPointerEnter={() => onInspect(row.key)}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          toggle();
        }
      }}
      className={cn(
        'relative grid min-h-[var(--row-height-data)] cursor-pointer grid-cols-1 items-center gap-y-1 border-b border-edge-subtle/60 px-3 py-1.5 text-left outline-none',
        COLUMNS,
        COLUMN_GAP,
        'hover:bg-surface-1 focus-visible:bg-surface-1',
        inspected && !selected && 'bg-surface-1',
        selected && 'bg-surface-2',
        'scroll-mt-16',
      )}
    >
      <span
        aria-hidden
        className={cn(
          'absolute inset-y-0 left-0 w-[3px]',
          selected ? 'bg-accent' : 'bg-transparent',
        )}
      />
      <Cell column="key">
        <span
          className="td-value block min-w-0 text-2xs [overflow-wrap:anywhere]"
          title={row.row.description ?? undefined}
        >
          <KeyText value={row.key} query={query} />
        </span>
      </Cell>
      <Cell column="value">
        <ValueCell row={row.row} query={query} />
        {proposed !== null ? (
          <span className="mt-0.5 flex min-w-0 items-baseline gap-1.5 text-2xs">
            <span className="td-legend text-accent">proposed</span>
            <span className="td-value min-w-0 break-all text-text-primary">{proposed}</span>
          </span>
        ) : null}
      </Cell>
      <Cell column="provenance">
        <ProvenanceChip kind={provenance} />
      </Cell>
      <Cell column="origin">
        <OriginCell section={row.section} />
      </Cell>
      <Cell column="write">
        <WriteCell capability={capability} />
      </Cell>
    </div>
  );
}

/** One cell; in the stacked layout the column name is printed beside the value
 * so a row still reads as key/value/provenance/origin/write. */
function Cell({ column, children }: { column: string; children: ReactNode }) {
  return (
    <div
      role="gridcell"
      data-col={column}
      className="flex min-w-0 items-baseline gap-2 @min-[41rem]:block"
    >
      <span className="td-legend w-24 shrink-0 @min-[41rem]:hidden">{column}</span>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

/** The group's origin on every row, elided from the START: a path's tail is
 * what distinguishes two config files, and its head is shared boilerplate the
 * section row above already prints in full. */
function OriginCell({ section }: { section: ConfigSection }) {
  if (!section.location) {
    return (
      <span className="td-value text-2xs text-text-muted" title="origin not served">
        —<span className="sr-only">origin not served</span>
      </span>
    );
  }
  return (
    <span
      className={cn(
        'td-value block min-w-0 truncate text-2xs',
        section.locationKind === 'path' ? 'text-text-primary' : 'text-text-secondary',
      )}
      title={section.location}
    >
      {elideStart(section.location, 26)}
    </span>
  );
}
