import type { ReactNode } from "react";

import { Button } from "../sdk";
import { Spinner } from "../Spinner";

/**
 * Shared scaffold for the Curation sub-tab panels: tabpanel/aria wiring,
 * header with an optional Refresh button, an optional error banner, and the
 * scroll container.
 */
export function CurationTabPanel({
  tab,
  title,
  subtitle,
  refreshing = false,
  onRefresh,
  error = "",
  children,
}: {
  tab: string;
  title: string;
  subtitle: string;
  refreshing?: boolean;
  onRefresh?: () => void;
  error?: string;
  children: ReactNode;
}) {
  return (
    <div
      role="tabpanel"
      id={`curation-panel-${tab}`}
      aria-labelledby={`curation-tab-${tab}`}
      className="flex flex-1 min-h-0 flex-col gap-3 overflow-y-auto overflow-x-hidden pr-1"
    >
      <div className="flex min-w-0 items-center justify-between gap-2 shrink-0">
        <div className="min-w-0">
          <div className="text-xs font-medium text-foreground">{title}</div>
          <div className="text-[11px] text-text-tertiary">{subtitle}</div>
        </div>
        {onRefresh ? (
          <Button
            size="xs"
            ghost
            disabled={refreshing}
            onClick={onRefresh}
            className="shrink-0 gap-2"
          >
            {refreshing ? <Spinner /> : null}
            Refresh
          </Button>
        ) : null}
      </div>
      {error ? (
        <div className="border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive shrink-0">
          {error}
        </div>
      ) : null}
      {children}
    </div>
  );
}
