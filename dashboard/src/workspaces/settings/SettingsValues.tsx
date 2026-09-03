/**
 * Value rendering for the settings surface.
 *
 * Everything here draws a literal the read model already found in the payload:
 * a key/value row, an environment override, a matched substring, a path. It
 * decides nothing about what a value means — the kind it renders by is the
 * kind `settingsModel` classified it as.
 */

import { useMemo, type ReactNode } from 'react';
import { cn } from '../../ui/cn';
import { Lamp } from '../../ui/instrument.tsx';
import {
  countSettings,
  splitPath,
  type ConfigRow,
  type ConfigSection,
  type EnvOverride,
  type OriginKind,
} from './settingsModel.ts';

const ORIGIN_GLYPH: Readonly<Record<OriginKind, string>> = {
  file: 'F',
  environment: 'E',
  resolved: 'R',
};

export const ORIGIN_WORD: Readonly<Record<OriginKind, string>> = {
  file: 'from file',
  environment: 'process environment',
  resolved: 'daemon-resolved',
};

/** Origin as an engraved initial. Decorative — every use sits beside the word. */
export function OriginMark({ origin }: { origin: OriginKind }) {
  return (
    <span
      aria-hidden
      className={cn(
        'td-value flex size-4 shrink-0 items-center justify-center border text-3xs',
        origin === 'resolved'
          ? 'border-edge-subtle text-text-muted'
          : 'border-edge-strong text-text-secondary',
      )}
    >
      {ORIGIN_GLYPH[origin]}
    </span>
  );
}

/* ---------------------------------------------------------------- section --*/

export function ConfigSectionBlock({
  section,
  rows,
  overrides,
  query,
}: {
  section: ConfigSection;
  rows: ConfigRow[];
  overrides: readonly EnvOverride[];
  query: string;
}) {
  const headingId = `settings-${section.id}-heading`;
  return (
    <section data-section={section.id} aria-labelledby={headingId} className="min-w-0">
      <header className="sticky top-0 z-10 flex flex-wrap items-center gap-x-2.5 gap-y-0.5 border-y border-edge-subtle bg-surface-2 px-3 py-1.5">
        <OriginMark origin={section.origin} />
        <h2 id={headingId} className="text-xs font-semibold tracking-tight">
          {section.title}
        </h2>
        <span className="td-legend">{ORIGIN_WORD[section.origin]}</span>
        {section.location ? (
          <span className="td-value min-w-0 truncate text-3xs">
            {section.locationKind === 'path' ? (
              <PathText value={section.location} />
            ) : (
              <span className="text-text-secondary">{section.location}</span>
            )}
          </span>
        ) : null}
        <span className="td-value ml-auto shrink-0 text-3xs text-text-muted">
          {countSettings(rows)}
        </span>
      </header>
      <div className="px-3 py-2">
        {overrides.length > 0 ? (
          <OverrideList overrides={overrides} query={query} />
        ) : null}
        <RowGroup rows={rows} query={query} start={0} depth={0} />
      </div>
    </section>
  );
}

/* --------------------------------------------------------------- overrides --*/

/**
 * The only genuine per-value provenance on the wire: for each variable the
 * daemon reports, whether it is set in the process environment (an override in
 * force, with its literal value) or unset (so a default applies). Active first,
 * because an override in force is the thing worth finding.
 *
 * The state is carried by the word "in force" / "unset" as much as by the lamp
 * — colour never states it alone.
 */
function OverrideList({
  overrides,
  query,
}: {
  overrides: readonly EnvOverride[];
  query: string;
}) {
  const ordered = useMemo(
    () => [...overrides].sort((a, b) => Number(b.active) - Number(a.active)),
    [overrides],
  );
  return (
    <div className="mb-3">
      <h3 className="mb-1 flex items-center gap-2.5 border-b border-edge-subtle pb-1">
        <span className="td-title">Overrides</span>
        <span aria-hidden className="td-rule" />
        <span className="td-value shrink-0 text-3xs text-text-muted">
          {ordered.filter((item) => item.active).length}/{ordered.length} in force
        </span>
      </h3>
      <ul className="flex flex-col">
        {ordered.map((item) => (
          <li
            key={item.name}
            className="grid grid-cols-1 gap-x-3 gap-y-0.5 border-b border-edge-subtle/60 py-1.5 last:border-b-0 md:grid-cols-[minmax(6rem,15rem)_minmax(0,1fr)]"
          >
            <div className="flex min-w-0 items-center gap-1.5">
              <Lamp tone={item.active ? 'bg-state-ready' : 'bg-surface-3'} />
              <span className="td-value min-w-0 break-all text-2xs text-text-primary">
                <Highlight text={item.name} query={query} />
              </span>
            </div>
            <div className="flex min-w-0 flex-col gap-0.5">
              <span className="flex min-w-0 flex-wrap items-baseline gap-x-2">
                <span
                  className={cn(
                    'td-legend',
                    item.active ? 'text-text-secondary' : 'text-text-muted',
                  )}
                >
                  {item.active ? 'in force' : 'unset · default applies'}
                </span>
                {item.value != null ? (
                  <span className="td-value min-w-0 break-all text-2xs text-text-primary">
                    <Highlight text={item.value} query={query} />
                  </span>
                ) : null}
              </span>
              {item.description ? (
                <span className="text-2xs leading-snug text-text-muted">
                  <Highlight text={item.description} query={query} />
                </span>
              ) : null}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

/* -------------------------------------------------------------------- rows --*/

/**
 * Renders one nesting level: consecutive leaf rows share a `<dl>` so the key
 * column aligns, and each nested group opens its own titled block. (A heading
 * cannot live inside a `<dl>`, so groups break the list rather than nest in it.)
 *
 * Nesting is expressed as an indent under a heading rather than as a nested
 * label/value grid — the same lesson `KeyValueTree` learned the hard way: a
 * per-level label track compounds until the value column measures 0px. Here
 * exactly one label track is ever reserved, at any depth.
 */
function RowGroup({
  rows,
  query,
  start,
  depth,
}: {
  rows: ConfigRow[];
  query: string;
  start: number;
  depth: number;
}) {
  const blocks: ReactNode[] = [];
  let leaves: ConfigRow[] = [];
  const flushLeaves = (key: string) => {
    if (leaves.length === 0) return;
    const batch = leaves;
    leaves = [];
    blocks.push(
      <dl key={key} className="flex flex-col">
        {batch.map((row) => (
          <ValueRow key={row.id} row={row} query={query} />
        ))}
      </dl>,
    );
  };

  for (let index = start; index < rows.length; index += 1) {
    const row = rows[index]!;
    if (row.depth < depth) break;
    if (row.depth > depth) continue;
    if (row.kind === 'group') {
      flushLeaves(`leaves-${row.id}`);
      blocks.push(
        <div key={row.id} className="mt-2 first:mt-0">
          <h3 className="flex items-baseline gap-2 border-b border-edge-subtle pb-1">
            <span className="td-value text-2xs font-semibold text-text-secondary">
              <Highlight text={row.label} query={query} />
            </span>
            <span className="td-value text-3xs text-text-muted">
              {row.count} {row.count === 1 ? 'value' : 'values'}
            </span>
          </h3>
          <div className="border-l border-edge-subtle pl-2.5 pt-1">
            <RowGroup rows={rows} query={query} start={index + 1} depth={depth + 1} />
          </div>
        </div>,
      );
    } else {
      leaves.push(row);
    }
  }
  flushLeaves('leaves-tail');
  return <>{blocks}</>;
}

/** One key/value pair: aligned columns on wide viewports, stacked on narrow. */
function ValueRow({ row, query }: { row: ConfigRow; query: string }) {
  return (
    <div className="grid grid-cols-1 gap-x-4 gap-y-0.5 border-b border-edge-subtle/60 py-1 last:border-b-0 md:grid-cols-[minmax(6rem,13rem)_minmax(0,1fr)] md:items-baseline">
      <dt className="td-legend min-w-0 truncate normal-case tracking-normal" title={row.id}>
        <Highlight text={row.label} query={query} />
      </dt>
      <dd className="min-w-0">
        <ValueCell row={row} query={query} />
      </dd>
    </div>
  );
}

/** Typed at render, by the kind the read model classified — never re-guessed. */
function ValueCell({ row, query }: { row: ConfigRow; query: string }) {
  switch (row.kind) {
    case 'boolean': {
      const on = row.value === true;
      return (
        <span
          className={cn(
            'td-value inline-flex items-center gap-1.5 border px-1.5 py-px text-2xs',
            on ? 'border-edge-strong text-text-primary' : 'border-edge-subtle text-text-muted',
          )}
        >
          <Lamp tone={on ? 'bg-state-ready' : 'bg-surface-3'} />
          {on ? 'true' : 'false'}
        </span>
      );
    }
    case 'number':
      return (
        <span className="td-value text-2xs text-text-primary" data-cell="numeric">
          {typeof row.value === 'number' ? row.value.toLocaleString() : row.text}
        </span>
      );
    case 'null':
      return <span className="td-value text-2xs text-text-muted">null</span>;
    case 'path':
      return (
        <span className="td-value block min-w-0 break-all text-2xs">
          <PathText value={String(row.value)} query={query} />
        </span>
      );
    case 'list': {
      const items = Array.isArray(row.value) ? row.value : [];
      if (items.length === 0) {
        return <span className="text-2xs text-text-muted">{row.text}</span>;
      }
      return (
        <span className="flex flex-wrap gap-1">
          {items.map((item, index) => (
            <span
              key={`${String(item)}-${index}`}
              className="td-value border border-edge-subtle bg-surface-2 px-1.5 py-px text-2xs text-text-secondary"
            >
              <Highlight text={String(item)} query={query} />
            </span>
          ))}
        </span>
      );
    }
    // A group's own line is drawn by `RowGroup`; if one reaches a value cell it
    // renders its search text like any other, rather than inventing a summary.
    case 'group':
    case 'string':
      return (
        <span className="td-value block min-w-0 break-words text-2xs text-text-primary">
          <Highlight text={row.text} query={query} />
        </span>
      );
    default: {
      const exhaustive: never = row.kind;
      return exhaustive;
    }
  }
}

/** A path reads from its tail: dim the directory, keep the last segment bright. */
export function PathText({ value, query = '' }: { value: string; query?: string }) {
  const { head, tail } = splitPath(value);
  return (
    <>
      {head ? (
        <span className="text-text-muted">
          <Highlight text={head} query={query} />
        </span>
      ) : null}
      <span className="text-text-primary">
        <Highlight text={tail} query={query} />
      </span>
    </>
  );
}

/** Marks every occurrence of the active filter inside a literal. */
function Highlight({ text, query }: { text: string; query: string }) {
  const needle = query.trim().toLowerCase();
  if (needle === '') return <>{text}</>;
  const parts: ReactNode[] = [];
  const haystack = text.toLowerCase();
  let cursor = 0;
  let found = haystack.indexOf(needle, cursor);
  while (found >= 0) {
    if (found > cursor) parts.push(text.slice(cursor, found));
    parts.push(
      <mark
        key={`${found}`}
        className="bg-accent/25 px-px text-text-primary underline decoration-accent decoration-1 underline-offset-2"
      >
        {text.slice(found, found + needle.length)}
      </mark>,
    );
    cursor = found + needle.length;
    found = haystack.indexOf(needle, cursor);
  }
  if (cursor < text.length) parts.push(text.slice(cursor));
  return <>{parts}</>;
}
