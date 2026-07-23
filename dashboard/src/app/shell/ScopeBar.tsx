import { Command, Moon, Sun } from 'lucide-react';
import { cn } from '../../ui/cn';

function toggleTheme() {
  const root = document.documentElement;
  const next = root.dataset['theme'] === 'light' ? 'dark' : 'light';
  root.dataset['theme'] = next;
  localStorage.setItem('td-theme', next);
}

/** Always-visible active scope (plan 11: every view preserves and displays
 * scope; transitions are explicit). Chips become interactive once the scope
 * store lands; the bar ships first so no surface ever renders scopeless. */
export function ScopeBar({ onOpenPalette }: { onOpenPalette?: () => void }) {
  return (
    <header className="flex h-11 shrink-0 items-center gap-2 border-b border-edge-subtle bg-surface-1 px-3">
      <div className="flex min-w-0 flex-1 items-center gap-1.5" aria-label="Active scope">
        <ScopeChip label="profile" value="local" />
        <ScopeChip label="project" value="—" muted />
      </div>
      <button
        type="button"
        onClick={onOpenPalette}
        className={cn(
          'flex h-7 items-center gap-1.5 rounded-[var(--radius-standard)] border border-edge-subtle',
          'bg-surface-2 px-2 text-2xs text-text-muted hover:text-text-secondary',
        )}
        aria-label="Open command palette"
      >
        <Command aria-hidden size={12} />
        <span>K</span>
      </button>
      <button
        type="button"
        onClick={toggleTheme}
        aria-label="Toggle theme"
        className="flex size-7 items-center justify-center rounded-[var(--radius-standard)] text-text-muted hover:bg-surface-2 hover:text-text-primary"
      >
        <Sun aria-hidden size={14} className="hidden [[data-theme=light]_&]:block" />
        <Moon aria-hidden size={14} className="[[data-theme=light]_&]:hidden" />
      </button>
    </header>
  );
}

function ScopeChip({ label, value, muted }: { label: string; value: string; muted?: boolean }) {
  return (
    <span
      className={cn(
        'inline-flex h-6 items-center gap-1 rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-2 px-2 text-2xs',
        muted ? 'text-text-muted' : 'text-text-secondary',
      )}
    >
      <span className="text-text-muted">{label}</span>
      <span className="font-medium">{value}</span>
    </span>
  );
}
