import { Button } from "../sdk";
import { Spinner } from "../Spinner";

/**
 * Shared loading / error presentation for the holographic panels:
 * loading announces itself to assistive tech; errors are humanized
 * ("Failed to fetch" → can't-reach-server) and always offer a retry.
 */

export function PanelLoading({ label = "Loading…" }: { label?: string }) {
  return (
    <div
      className="flex items-center justify-center gap-2 py-16"
      role="status"
      aria-live="polite"
    >
      <Spinner className="text-2xl text-primary" />
      <span className="text-xs text-text-tertiary">{label}</span>
    </div>
  );
}

function humanize(error: string): string {
  if (/failed to fetch|networkerror|load failed/i.test(error)) {
    return "Can't reach the tracedecay server. It may be restarting — retry in a moment.";
  }
  return error;
}

export function PanelError({
  error,
  onRetry,
}: {
  error: string;
  onRetry?: () => void;
}) {
  return (
    <div
      className="flex flex-wrap items-center gap-3 border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive"
      role="alert"
    >
      <span className="min-w-0">{humanize(error)}</span>
      {onRetry && (
        <Button size="xs" outlined onClick={onRetry} className="ml-auto shrink-0">
          Retry
        </Button>
      )}
    </div>
  );
}
