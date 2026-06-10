export function MiniBar({
  pct,
  className,
  color,
}: {
  pct: number;
  className?: string;
  /** CSS color (token var) for the filled portion; defaults to the foreground bar. */
  color?: string;
}) {
  return (
    <div className={`h-1.5 bg-muted ${className ?? ""}`}>
      <div
        className={color ? "h-full" : "h-full bg-midground"}
        style={{ width: `${Math.max(4, pct)}%`, ...(color ? { background: color } : null) }}
      />
    </div>
  );
}
