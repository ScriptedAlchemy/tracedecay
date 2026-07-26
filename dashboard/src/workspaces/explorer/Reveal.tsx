import { useEffect, useState, type ReactNode } from 'react';
import { cn } from '../../ui/cn';

/**
 * The surface's single orchestrated entrance.
 *
 * Explorer settles in one wave rather than a scatter of independent
 * micro-interactions: each region rises a hair and resolves, one `--dur-state`
 * behind the region before it, so the eye is led left-to-right through the
 * lane readouts and down the query rail exactly once. Nothing re-plays on
 * keystrokes — the state is seeded on mount and never re-armed.
 *
 * Reduced motion is a real no-motion path, not a shortened one: the
 * `motion-reduce` utilities resolve to the settled values, so there is no
 * transition to run and no frame in which content is hidden.
 */
export function Reveal({
  index = 0,
  className,
  children,
}: {
  /** Position in the wave. Each step delays by one `--dur-state`. */
  index?: number;
  className?: string;
  children: ReactNode;
}) {
  const [entered, setEntered] = useState(false);
  useEffect(() => {
    const frame = requestAnimationFrame(() => setEntered(true));
    return () => cancelAnimationFrame(frame);
  }, []);
  return (
    <div
      className={cn(
        'transition-[opacity,transform] duration-[var(--dur-panel)] ease-[var(--ease-standard)]',
        entered ? 'translate-y-0 opacity-100' : 'translate-y-1 opacity-0',
        'motion-reduce:translate-y-0 motion-reduce:opacity-100 motion-reduce:transition-none',
        className,
      )}
      style={{ transitionDelay: `calc(${index} * var(--dur-state))` }}
    >
      {children}
    </div>
  );
}
