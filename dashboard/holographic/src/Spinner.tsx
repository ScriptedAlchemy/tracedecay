import { useEffect, useState, type CSSProperties } from "react";

/**
 * Minimal braille spinner — local stand-in for `@nous-research/ui`'s `Spinner`,
 * which the host does not expose on the plugin SDK. Inherits font color/size
 * from its parent; style via `className` (e.g. `text-primary`).
 */
const FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

interface SpinnerProps {
  className?: string;
  style?: CSSProperties;
  "aria-label"?: string;
}

export function Spinner({ className, style, ...props }: SpinnerProps) {
  const [frame, setFrame] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setFrame((f) => (f + 1) % FRAMES.length), 80);
    return () => clearInterval(id);
  }, []);
  return (
    <span
      aria-hidden={props["aria-label"] ? undefined : true}
      className={`font-mono inline-block leading-none tabular-nums ${className ?? ""}`}
      style={style}
      {...props}
    >
      {FRAMES[frame]}
    </span>
  );
}
