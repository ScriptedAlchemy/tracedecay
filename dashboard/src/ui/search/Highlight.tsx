import { Fragment } from 'react';
import { cn } from '../cn';
import { segmentMatches } from './terms.ts';

/**
 * Renders `text` with the submitted query terms marked in place. The mark is a
 * tinted ground plus a weight shift, so the signal survives greyscale and
 * forced-colors and never relies on the tint alone.
 */
export function Highlight({
  text,
  terms,
  className,
}: {
  text: string;
  terms: readonly string[];
  className?: string;
}) {
  const segments = segmentMatches(text, terms);
  return (
    <span className={className}>
      {segments.map((segment, i) =>
        segment.hit ? (
          <mark
            key={i}
            className="rounded-[2px] bg-accent/25 px-[0.15em] font-semibold text-text-primary underline decoration-accent decoration-2 underline-offset-2"
          >
            {segment.text}
          </mark>
        ) : (
          <Fragment key={i}>{segment.text}</Fragment>
        ),
      )}
    </span>
  );
}

/** Small caps-ish label used to name a facet, a field, or a lane. */
export function MetaLabel({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <span
      className={cn(
        'text-2xs font-medium uppercase tracking-[0.08em] text-text-muted',
        className,
      )}
    >
      {children}
    </span>
  );
}
