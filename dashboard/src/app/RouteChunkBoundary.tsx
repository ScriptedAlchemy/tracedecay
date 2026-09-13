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
  /** The loader the current `error`/`attempt` belong to. Every route mounts
   * this same component type at the same Outlet position, so React reconciles
   * a navigation as a PROP change — the boundary must notice the new loader
   * itself rather than assuming a fresh mount. */
  loadedFor: RouteChunkLoader;
};

/**
 * One lazy component per loader, shared across renders and navigations.
 *
 * `lazy()` memoizes its resolved module on the instance, so a per-loader cache
 * means revisiting a workspace renders it synchronously instead of suspending
 * again. A rejected import is also cached on the instance, which is why
 * `retry` evicts the entry and allocates a fresh one.
 */
const CHUNKS = new WeakMap<RouteChunkLoader, LazyExoticComponent<ComponentType>>();

function lazyFor(load: RouteChunkLoader): LazyExoticComponent<ComponentType> {
  let page = CHUNKS.get(load);
  if (page === undefined) {
    page = lazy(load);
    CHUNKS.set(load, page);
  }
  return page;
}

/**
 * Per-route lazy boundary.
 *
 * THE PROP CAN CHANGE WITHOUT A REMOUNT. React Router keeps the `<Outlet>`
 * slot's component type identical across workspace navigations, so switching
 * from `/brain` to `/code` updates `load` on the existing instance. The first
 * version of this class computed `lazy(props.load)` once in its constructor,
 * which pinned every client-side navigation to whichever workspace loaded
 * first — the URL and the nav rail moved, the content never did.
 */
export class RouteChunkBoundary extends Component<
  RouteChunkBoundaryProps,
  RouteChunkBoundaryState
> {
  constructor(props: RouteChunkBoundaryProps) {
    super(props);
    this.state = { error: null, attempt: 0, loadedFor: props.load };
  }

  static getDerivedStateFromError(error: unknown): Pick<RouteChunkBoundaryState, 'error'> {
    return { error: asError(error) };
  }

  static getDerivedStateFromProps(
    props: RouteChunkBoundaryProps,
    state: RouteChunkBoundaryState,
  ): Partial<RouteChunkBoundaryState> | null {
    // A navigation to a different route: drop the previous route's failure so
    // one broken chunk does not paint every workspace unavailable.
    if (props.load !== state.loadedFor) {
      return { error: null, attempt: 0, loadedFor: props.load };
    }
    return null;
  }

  private retry = () => {
    CHUNKS.delete(this.props.load);
    this.setState((state) => ({ error: null, attempt: state.attempt + 1 }));
  };

  render(): ReactNode {
    if (this.state.error) {
      return <RouteChunkUnavailable error={this.state.error} onRetry={this.retry} />;
    }
    const Page = lazyFor(this.props.load);
    return (
      <Suspense fallback={<ChunkFallback />}>
        <Page key={this.state.attempt} />
      </Suspense>
    );
  }
}
