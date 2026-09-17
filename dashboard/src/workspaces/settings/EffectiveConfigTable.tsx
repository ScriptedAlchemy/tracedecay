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

import { Lock, PenLine } from 'lucide-react';
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
import { Lamp } from '../../ui/instrument.tsx';
import type { SettingsEditorHandle } from './SettingsEditorController.tsx';
import type { SettingsEditorState } from './settingsEditorMachine.ts';
import type { WritableScopes } from './settingsGates.ts';
import { selectionText, type ConfigSection, type ServedProvenance } from './settingsModel.ts';
import { SettingsReviewPanel } from './SettingsReviewPanel.tsx';
import {
  bindingFor,
  draftValue,
  fieldEdited,
  writeCapability,
  type EffectiveRow,
  type WriteCapability,
} from './settingsRows.ts';
import { KeyText, ORIGIN_WORD, OriginMark, PathText, ValueCell } from './SettingsValues.tsx';

export interface SectionGroup {
  readonly section: ConfigSection;
  readonly rows: readonly EffectiveRow[];
}

/** The provenance column's vocabulary: what the wire served, plus the one
 * client-side state — a proposal not yet applied — that is never confused
 * with it. */
export type RowProvenance = ServedProvenance | 'edited';

/**
 * Column tracks switch on the TABLE's own width, not the viewport's: at `lg`
 * the table sits between the sections rail and the inspector and may be
 * narrower than it is at `md` with both stacked. Below `@xl` (36rem) each row
 * stacks its cells with the column name printed beside each value.
 */
const COLUMNS =
  '@xl:grid-cols-[minmax(10rem,1.3fr)_minmax(8rem,1.4fr)_6rem_minmax(8rem,1fr)_4.5rem]';

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
  const firstKey = groups[0]?.rows[0]?.key ?? null;
  const selectedVisible = groups.some((group) => group.rows.some((row) => row.key === selectedKey));
  const tabbableKey = selectedVisible ? selectedKey : firstKey;

  // Focus returns to the edited row when a frozen review resolves: the Apply,
  // Retry, or Load-current-values control the reader was on unmounts with the
  // review stage, and focus dropped to the document body — which is where the
  // next Escape or arrow key would have gone unheard.
  const status = editor.state.status;
  const previousStatus = useRef(status);
  useEffect(() => {
    const wasStaged = REVIEW_BEARING.has(previousStatus.current);
    previousStatus.current = status;
    if (wasStaged && status === 'editing' && selectedKey !== null) {
      focusRow(scrollRef.current, selectedKey);
    }
  }, [status, selectedKey, scrollRef]);

  const onKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      const container = scrollRef.current;
      if (!container) return;
      if (event.key === 'Escape') {
        if (selectedKey !== null) {
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
    [scrollRef, selectedKey, inspectedKey, onSelect, onInspect],
  );

  return (
    <div
      ref={scrollRef}
      role="grid"
      aria-label="Effective configuration"
      aria-rowcount={groups.reduce((total, group) => total + group.rows.length, 0)}
      onKeyDown={onKeyDown}
      className="@container relative min-h-[var(--pane-min-height)] min-w-0 flex-1 overflow-auto td-well"
    >
      <div
        role="row"
        className={cn(
          'sticky top-0 z-20 hidden border-b border-edge-subtle bg-surface-1 px-3 @xl:grid',
          COLUMNS,
        )}
      >
        {['key', 'value', 'provenance', 'origin', 'write'].map((column) => (
          <span key={column} role="columnheader" className="td-legend py-2 pr-3">
            {column}
          </span>
        ))}
      </div>
      {groups.map(({ section, rows }) => (
        <div key={section.id} role="rowgroup" data-section={section.id} className="min-w-0">
          <SectionRow section={section} count={rows.length} />
          {rows.map((row) => {
            const capability = writeCapability(row.key, gates);
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

/** The section's own line: origin glyph and word, the location the payload
 * names, and how many of its keys are shown under the current filter. */
function SectionRow({ section, count }: { section: ConfigSection; count: number }) {
  const headingId = `settings-${section.id}-heading`;
  return (
    <div
      role="row"
      className="sticky top-0 z-10 flex flex-wrap items-center gap-x-2.5 gap-y-0.5 border-y border-edge-subtle bg-surface-2 px-3 py-1.5 @xl:top-7"
    >
      <div role="gridcell" aria-colspan={5} className="flex min-w-0 flex-1 flex-wrap items-center gap-x-2.5 gap-y-0.5">
        <OriginMark origin={section.origin} />
        <h2 id={headingId} className="text-xs font-semibold tracking-tight">
          {section.title}
        </h2>
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
          <span key={note} className="border border-edge-subtle px-1.5 py-px text-3xs text-text-secondary">
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
  const toggle = () => onSelect(selected ? null : row.key);
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
        'relative grid min-h-[var(--row-height-data)] cursor-pointer grid-cols-1 items-center gap-x-3 gap-y-1 border-b border-edge-subtle/60 px-3 py-1.5 text-left outline-none',
        COLUMNS,
        'hover:bg-surface-1 focus-visible:bg-surface-1',
        inspected && !selected && 'bg-surface-1',
        selected && 'bg-surface-2',
        'scroll-mt-16',
      )}
    >
      <span
        aria-hidden
        className={cn('absolute inset-y-0 left-0 w-[3px]', selected ? 'bg-accent' : 'bg-transparent')}
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

/** One cell; below `md` the column name is printed beside the value so a
 * stacked row still reads as key/value/provenance/origin/write. */
function Cell({ column, children }: { column: string; children: ReactNode }) {
  return (
    <div role="gridcell" data-col={column} className="flex min-w-0 items-baseline gap-2 @xl:block @xl:pr-3">
      <span className="td-legend w-24 shrink-0 @xl:hidden">{column}</span>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

/**
 * The typed-state treatment for provenance, per the design system's ladder:
 * `unserved` is the gray dashed disconnected family (the layer is not on the
 * wire), `explicit` and `default` are solid served facts, `edited` is cyan
 * because it is this reader's own unapplied proposal. Colour never carries the
 * state alone: every chip prints its word.
 */
export function ProvenanceChip({ kind }: { kind: RowProvenance }) {
  switch (kind) {
    case 'unserved':
      return (
        <span
          className="td-value inline-flex items-center gap-1.5 border border-dashed border-edge-strong px-1.5 py-px text-3xs text-text-muted"
          title="the API states the effective value but not the layer that supplied it"
        >
          unserved
        </span>
      );
    case 'explicit':
      return (
        <span
          className="td-value inline-flex items-center gap-1.5 border border-edge-strong px-1.5 py-px text-3xs text-text-primary"
          title="set in the daemon's process environment: this override is in force"
        >
          <Lamp tone="bg-state-ready" />
          explicit
        </span>
      );
    case 'default':
      return (
        <span
          className="td-value inline-flex items-center gap-1.5 border border-edge-subtle px-1.5 py-px text-3xs text-text-muted"
          title="unset in the daemon's process environment: the default applies"
        >
          <Lamp tone="bg-surface-3" />
          default
        </span>
      );
    case 'edited':
      return (
        <span
          className="td-value inline-flex items-center gap-1.5 border border-accent px-1.5 py-px text-3xs text-accent"
          title="your proposal differs from the effective value and is not applied"
        >
          edited
        </span>
      );
    default: {
      const exhaustive: never = kind;
      return exhaustive;
    }
  }
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
      {elideStart(section.location, 30)}
    </span>
  );
}

export function WriteCell({ capability }: { capability: WriteCapability }) {
  switch (capability.kind) {
    case 'writable':
      return (
        <span className="inline-flex items-center gap-1 text-2xs text-text-secondary">
          <PenLine aria-hidden size={11} />
          editable
        </span>
      );
    case 'locked':
      return (
        <span className="inline-flex items-center gap-1 text-2xs text-state-locked" title={capability.reason}>
          <Lock aria-hidden size={11} />
          locked
        </span>
      );
    case 'no_write_path':
      return (
        <span className="td-value text-2xs text-text-muted" title="no write path">
          —<span className="sr-only">no write path</span>
        </span>
      );
    default: {
      const exhaustive: never = capability;
      return exhaustive;
    }
  }
}

function proposalText(value: unknown): string {
  if (Array.isArray(value)) return value.length === 0 ? 'empty list' : value.map(String).join(', ');
  if (typeof value === 'object' && value !== null && 'mode' in value) {
    const selection = value as { mode: 'automatic' } | { mode: 'exact'; workers: number };
    return selectionText(selection);
  }
  return String(value);
}
