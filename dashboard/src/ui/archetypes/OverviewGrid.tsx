import type { ReactNode } from 'react';
import { cn } from '../cn';

/** Archetype 1 (plan 11a): responsive card grid. Each card is one read model
 * with its truth strip; no card renders a computed grade. */
export function OverviewGrid({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        'grid gap-3 p-4',
        'grid-cols-1 md:grid-cols-2 xl:grid-cols-3',
        className,
      )}
    >
      {children}
    </div>
  );
}

export function OverviewCard({
  title,
  actions,
  children,
  footer,
  className,
}: {
  title: string;
  actions?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  className?: string;
}) {
  return (
    <section
      className={cn(
        'flex min-w-0 flex-col gap-2 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-4',
        className,
      )}
      aria-label={title}
    >
      <header className="flex items-center justify-between gap-2">
        <h2 className="truncate text-sm font-semibold tracking-tight">{title}</h2>
        {actions}
      </header>
      <div className="min-w-0 flex-1">{children}</div>
      {footer ? <footer className="border-t border-edge-subtle pt-2">{footer}</footer> : null}
    </section>
  );
}
