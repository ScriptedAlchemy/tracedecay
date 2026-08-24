import type { ReactNode } from 'react';
import { cn } from '../cn';
import { Panel } from '../instrument.tsx';

/** Responsive panel grid. Each panel is one read model with its truth
 * strip; no panel renders a computed grade.
 *
 * The gutter is one grid cell so panels land on the same rhythm the graticule
 * and the readout bars use — the whole console is ruled to a single module. */
export function OverviewGrid({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn('grid gap-2 p-2', 'grid-cols-1 md:grid-cols-2 xl:grid-cols-3', className)}
    >
      {children}
    </div>
  );
}

/** One bracketed instrument panel in the grid. `title` becomes the engraved
 * legend and the region's accessible name, exactly as before. */
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
    <Panel legend={title} actions={actions} footer={footer} className={className}>
      {children}
    </Panel>
  );
}
