import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { Outlet, useLocation } from 'react-router';
import { channelForPathname } from '../channels.ts';
import { NavRail } from './NavRail';
import { ScopeBar } from './ScopeBar';
import { ScopeUrlSync } from '../../data/scope/UrlSync.tsx';
import {
  QueryActivityStatus,
  RegistryAuthorityStatus,
  SourceProvenance,
  StatusStrip,
} from './StatusStrip';

const CommandPalette = lazy(() =>
  import('./CommandPalette').then((m) => ({ default: m.CommandPalette })),
);

/** Global Cmd/Ctrl-K binding, kept out of CommandPalette so the dialog chunk
 * stays out of the initial shell payload until the palette is first opened.
 * The only global shortcut (NAVIGATION.md "Behavior"): no numeric or arrow
 * keys are bound shell-wide. */
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

/**
 * The persistent shell (NAVIGATION.md "Persistent regions"): one framed
 * night-glass instrument set into the chassis, navigation rail | (scope
 * register / main aperture / status strip). The frame is the one place
 * outside a hero aperture that carries a bezel: a cyan-gray hairline and four
 * corner marks, and nothing else.
 *
 * A workspace's inspector mounts inside its own content (archetype-owned), so
 * its width interacts with the content grid, not the shell.
 */
export function Shell() {
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteMounted, setPaletteMounted] = useState(false);
  const openPalette = useCallback((open: boolean) => {
    if (open) setPaletteMounted(true);
    setPaletteOpen(open);
  }, []);
  usePaletteHotkey(openPalette);
  const { pathname } = useLocation();
  const channel = channelForPathname(pathname);
  return (
    // The chassis pads the frame at desktop widths only: below `md` the rail
    // is already compact and every pixel goes to the aperture.
    <div className="td-chassis h-dvh w-full md:p-[6px]">
      <div className="relative flex h-full w-full overflow-hidden border border-edge-frame bg-surface-0 text-text-primary">
        <span aria-hidden className="td-corners pointer-events-none absolute inset-[5px] z-10" />
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
          <ScopeBar channel={channel} onOpenPalette={() => openPalette(true)} />
          {/* Named because it is also the page's scroll container: a workspace
            * whose content outruns the viewport scrolls HERE rather than losing
            * the overflow, and Plan 11 licenses internal scrolling for labelled
            * regions only. */}
          <main
            id="td-main"
            aria-label="Active workspace"
            className="min-h-0 min-w-0 flex-1 overflow-auto"
          >
            <Outlet />
          </main>
          <StatusStrip
            queryActivity={
              <>
                <SourceProvenance />
                <QueryActivityStatus />
                <RegistryAuthorityStatus />
              </>
            }
          />
        </div>
      </div>
    </div>
  );
}
