import {
  Component,
  Suspense,
  lazy,
  type ComponentType,
  type LazyExoticComponent,
  type ReactNode,
} from 'react';
import { RefreshCw } from 'lucide-react';
import { Corners } from '../ui/instrument.tsx';
import { StateChip, type DomainStateKind } from '../ui/StateChip';

export type RouteChunkLoader = () => Promise<{ default: ComponentType }>;

const CHUNK_LOAD_MESSAGE =
  /Loading chunk|Failed to fetch dynamically imported module|error loading dynamically imported module/i;

export function isChunkLoadFailure(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return error.name === 'ChunkLoadError' || CHUNK_LOAD_MESSAGE.test(error.message);
}

type RouteChunkPresentation = {
  kind: DomainStateKind;
  title: string;
  detail: string;
};

function routeChunkPresentation(error: Error): RouteChunkPresentation {
  if (isChunkLoadFailure(error)) {
    return {
      kind: 'offline',
      title: 'Dashboard server unreachable',
      detail:
        "the dashboard server is unreachable; this page's script chunk could not be loaded",
    };
  }
  return {
    kind: 'unavailable',
    title: 'Workspace unavailable',
    detail: "this page's script chunk could not be loaded",
  };
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error("this page's script chunk could not be loaded");
}

function ChunkFallback() {
  return (
    <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
      <span className="text-sm font-semibold tracking-tight text-text-muted">Loading…</span>
    </div>
  );
}

function RouteChunkUnavailable({
  error,
  onRetry,
}: {
  error: Error;
  onRetry: () => void;
}) {
  const presentation = routeChunkPresentation(error);
  return (
    <div className="td-graticule flex h-full min-h-48 items-center justify-center bg-surface-0 p-8">
      <div className="relative flex max-w-md flex-col items-center gap-3 border border-edge-subtle bg-surface-1 px-8 py-6 text-center">
        <Corners />
        <h1 className="text-2xs font-semibold uppercase tracking-[0.2em] text-text-primary">
          {presentation.title}
        </h1>
        <span aria-hidden className="h-px w-10 bg-edge-strong" />
        <StateChip kind={presentation.kind} detail={presentation.detail} />
        <button type="button" className="td-hit group" onClick={onRetry}>
          <span className="inline-flex h-7 items-center gap-1.5 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 px-2.5 text-2xs font-medium text-text-secondary group-hover:text-text-primary">
            <RefreshCw aria-hidden size={12} />
            Retry
          </span>
        </button>
      </div>
    </div>
  );
}

type RouteChunkBoundaryProps = {
  load: RouteChunkLoader;
};

type RouteChunkBoundaryState = {
  error: Error | null;
  attempt: number;
};

/**
 * Per-route lazy boundary. React `lazy()` caches a rejected import on that
 * instance, so retry allocates a new `lazy(load)` and remounts it.
 */
export class RouteChunkBoundary extends Component<
  RouteChunkBoundaryProps,
  RouteChunkBoundaryState
> {
  state: RouteChunkBoundaryState = { error: null, attempt: 0 };
  private Page: LazyExoticComponent<ComponentType>;

  constructor(props: RouteChunkBoundaryProps) {
    super(props);
    this.Page = lazy(props.load);
  }

  static getDerivedStateFromError(error: unknown): Pick<RouteChunkBoundaryState, 'error'> {
    return { error: asError(error) };
  }

  private retry = () => {
    this.Page = lazy(this.props.load);
    this.setState((state) => ({ error: null, attempt: state.attempt + 1 }));
  };

  render(): ReactNode {
    if (this.state.error) {
      return <RouteChunkUnavailable error={this.state.error} onRetry={this.retry} />;
    }
    const Page = this.Page;
    return (
      <Suspense fallback={<ChunkFallback />}>
        <Page key={this.state.attempt} />
      </Suspense>
    );
  }
}
