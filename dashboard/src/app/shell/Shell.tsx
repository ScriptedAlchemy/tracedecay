import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { Outlet } from 'react-router';
import { NavRail } from './NavRail';
import { ScopeBar } from './ScopeBar';
import { ScopeUrlSync } from '../../data/scope/UrlSync.tsx';
import { QueryActivityStatus, StatusStrip } from './StatusStrip';
import { InspectorStack, InspectorUrlSync } from './InspectorStack.tsx';

const CommandPalette = lazy(() =>
  import('./CommandPalette').then((m) => ({ default: m.CommandPalette })),
);

/** Global Cmd/Ctrl-K binding — kept out of CommandPalette so the dialog chunk
 * stays out of the initial shell payload until the palette is first opened. */
function usePaletteHotkey(setOpen: (open: boolean) => void) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setOpen(true);
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [setOpen]);
}

/** Shell layout per plan 11a: nav rail | (scope bar / content / status strip).
 * The inspector panel mounts inside workspace content (archetype-owned) so
 * its width interacts with the content grid, not the shell. */
export function Shell() {
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteMounted, setPaletteMounted] = useState(false);
  const openPalette = useCallback((open: boolean) => {
    if (open) setPaletteMounted(true);
    setPaletteOpen(open);
  }, []);
  usePaletteHotkey(openPalette);
  return (
    <div className="flex h-dvh w-full overflow-hidden bg-surface-0 text-text-primary">
      <a
        href="#td-main"
        className="sr-only focus:not-sr-only focus:absolute focus:left-2 focus:top-2 focus:z-50 focus:rounded-[var(--radius-standard)] focus:bg-surface-3 focus:px-3 focus:py-2"
      >
        Skip to content
      </a>
      <NavRail />
      {paletteMounted ? (
        <Suspense fallback={null}>
          <CommandPalette open={paletteOpen} onOpenChange={openPalette} />
        </Suspense>
      ) : null}
      <div className="flex min-w-0 flex-1 flex-col">
        <ScopeUrlSync />
        <InspectorUrlSync />
        <ScopeBar onOpenPalette={() => openPalette(true)} />
        {/* Named because it is also the page's scroll container: a workspace
          * whose content outruns the viewport scrolls HERE rather than losing
          * the overflow, and Plan 11 licenses internal scrolling for labelled
          * regions only. */}
        <div className="relative flex min-h-0 flex-1">
          <main
            id="td-main"
            aria-label="Active workspace"
            className="min-h-0 min-w-0 flex-1 overflow-auto"
          >
            <Outlet />
          </main>
          <InspectorStack />
        </div>
        <StatusStrip queryActivity={<QueryActivityStatus />} />
      </div>
    </div>
  );
}
