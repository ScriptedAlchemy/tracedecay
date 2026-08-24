import type { ReactNode } from "react";

export function MetadataRow({
  label,
  value,
}: {
  label: string;
  value: ReactNode;
}) {
  return (
    <div className="flex min-w-0 items-center justify-between gap-3 border-b border-border/50 py-2 last:border-b-0">
      <span className="text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
        {label}
      </span>
      <span className="min-w-0 text-right font-mono-ui text-xs text-text-secondary break-all">
        {value}
      </span>
    </div>
  );
}
