import { useEffect, useId, useRef, type ReactNode } from 'react';
import { Search, X } from 'lucide-react';
import { cn } from '../cn';

/**
 * The primary search affordance for the finding surfaces. It is deliberately
 * the largest control on the page: on a surface whose whole job is retrieval,
 * the query field is the subject, not a widget parked in a filter rail.
 *
 * `/` focuses it from anywhere on the surface (ignored while another field has
 * focus), Escape clears back to the browse state.
 */
export function SearchField({
  value,
  onChange,
  onSubmit,
  onClear,
  label,
  placeholder,
  hint,
  submitted,
  children,
}: {
  value: string;
  onChange: (next: string) => void;
  onSubmit: () => void;
  onClear: () => void;
  label: string;
  placeholder: string;
  hint?: ReactNode;
  /** The query currently applied, used only to enable the clear affordance. */
  submitted: string;
  /** Trailing content rendered inside the field's row (lane summary, counts). */
  children?: ReactNode;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const hintId = useId();

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== '/' || event.metaKey || event.ctrlKey || event.altKey) return;
      const active = document.activeElement;
      const tag = active?.tagName.toLowerCase();
      if (tag === 'input' || tag === 'textarea' || tag === 'select') return;
      if (active instanceof HTMLElement && active.isContentEditable) return;
      event.preventDefault();
      inputRef.current?.focus();
      inputRef.current?.select();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const dirty = value !== '' || submitted !== '';

  return (
    <form
      role="search"
      className="flex min-w-0 flex-1 flex-col gap-1.5"
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <div
        className={cn(
          // The field is meant to be the largest control on the page and its
          // input measured 18.6px tall — the shell was 35px (`h-10` at a 14px
          // root) and the input only claimed its own line box inside it. The
          // shell now clears the touch minimum with its two hairlines counted,
          // and the input stretches into it rather than floating in the middle.
          'group flex min-h-[calc(var(--touch-target-min)+2px)] min-w-0 items-center gap-2 rounded-[var(--radius-standard)]',
          'border border-edge-subtle bg-surface-1 pl-3 pr-1.5',
          'focus-within:border-accent focus-within:bg-surface-0',
        )}
      >
        <Search aria-hidden size={15} className="shrink-0 text-text-muted" />
        <input
          type="search"
          ref={inputRef}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Escape' && dirty) {
              event.preventDefault();
              onClear();
            }
          }}
          placeholder={placeholder}
          aria-label={label}
          aria-describedby={hint ? hintId : undefined}
          spellCheck={false}
          autoComplete="off"
          className={cn(
            'min-w-0 flex-1 self-stretch bg-transparent text-sm text-text-primary outline-none',
            'placeholder:text-text-muted',
          )}
        />
        {dirty ? (
          <button
            type="button"
            onClick={onClear}
            aria-label="Clear search"
            // The × keeps its 14px glyph; only the hit area reaches the minimum.
            className="flex size-[var(--touch-target-min)] shrink-0 items-center justify-center rounded-[var(--radius-chip)] text-text-muted hover:bg-surface-2 hover:text-text-primary"
          >
            <X aria-hidden size={14} />
          </button>
        ) : null}
        <span
          aria-hidden
          className="hidden shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 py-0.5 text-2xs text-text-muted sm:inline"
        >
          {value === '' ? '/' : '↵'}
        </span>
      </div>
      {hint ? (
        <p id={hintId} className="text-2xs leading-relaxed text-text-muted">
          {hint}
        </p>
      ) : null}
      {children}
    </form>
  );
}
