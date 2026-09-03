/**
 * One reading, printed the same way wherever it appears.
 *
 * `readout.ts` makes an absent measurement unrepresentable-as-blank in the
 * TYPE. This is the single renderer of that type, which is what carries the
 * same property into the markup: the plate above the field and the key below it
 * both print readings, and a second copy of this would be a second answer to
 * "what does it look like when the wire was silent" — the two would agree until
 * one of them was edited.
 */
import { cn } from '../../ui/cn';
import type { ReadoutValue } from '../../viz/trace/readout.ts';

/**
 * Absence is printed as the word `absent` plus the reason, never as a blank
 * cell and never as a zero — a blank reads as "nothing to report" and a zero
 * reads as "none", and on this surface the truth is usually "the wire did not
 * carry it".
 */
export function TraceReading({ value, size }: { value: ReadoutValue; size: string }) {
  if (value.kind === 'absent') {
    return (
      <span className="flex min-w-0 flex-wrap items-baseline gap-x-1.5">
        <span className={cn('td-value text-state-unknown', size)}>absent</span>
        <span className="td-unit normal-case tracking-normal">{value.why}</span>
      </span>
    );
  }
  return (
    <span className="flex min-w-0 flex-wrap items-baseline gap-x-1.5">
      <span className={cn('td-value break-words', size)}>{value.value}</span>
      {value.unit === null ? null : <span className="td-unit">{value.unit}</span>}
    </span>
  );
}
