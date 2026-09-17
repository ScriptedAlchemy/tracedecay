/**
 * Value rendering for the settings surface.
 *
 * Everything here draws a literal the read model already found in the payload:
 * a typed value, a matched substring, a path, an origin glyph. It decides
 * nothing about what a value means — the kind it renders by is the kind
 * `settingsModel` classified it as.
 */

import { Fragment, type ReactNode } from 'react';
import { cn } from '../../ui/cn';
import { Lamp } from '../../ui/instrument.tsx';
import { splitPath, type ConfigRow, type OriginKind } from './settingsModel.ts';

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

/** Typed at render, by the kind the read model classified — never re-guessed. */
export function ValueCell({ row, query }: { row: ConfigRow; query: string }) {
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
    // A group's own line is never a value cell in the table; if one reaches
    // here it renders its search text like any other, rather than inventing a
    // summary. An environment variable that is unset renders the word the
    // model gave it, muted, so `unset` never reads as a literal value.
    case 'group':
    case 'selection':
    case 'string':
      return (
        <span
          className={cn(
            'td-value block min-w-0 break-words text-2xs',
            row.value === null ? 'text-text-muted' : 'text-text-primary',
          )}
        >
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

/**
 * A dotted key reads from its leaf: dim the prefix, keep the last segment
 * bright. A `<wbr>` after each dot gives a long key a real break point at a
 * segment boundary, so a narrow column wraps `project.config.` / `sync.…`
 * rather than mid-identifier.
 */
export function KeyText({ value, query = '' }: { value: string; query?: string }) {
  const cut = value.lastIndexOf('.');
  const head = cut < 0 ? '' : value.slice(0, cut + 1);
  const tail = cut < 0 ? value : value.slice(cut + 1);
  const segments = head.split('.').filter((segment) => segment.length > 0);
  return (
    <>
      {segments.length > 0 ? (
        <span className="text-text-muted">
          {segments.map((segment, index) => (
            <Fragment key={`${segment}-${index}`}>
              <Highlight text={`${segment}.`} query={query} />
              <wbr />
            </Fragment>
          ))}
        </span>
      ) : null}
      <span className="text-text-primary">
        <Highlight text={tail} query={query} />
      </span>
    </>
  );
}

/** Marks every occurrence of the active filter inside a literal. */
export function Highlight({ text, query }: { text: string; query: string }) {
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
