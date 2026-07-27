import { useState } from 'react';
import { Outlet } from 'react-router';
import { CommandPalette, usePaletteHotkey } from './CommandPalette';
import { NavRail } from './NavRail';
import { ScopeBar } from './ScopeBar';
import { ScopeUrlSync } from '../../data/scope/UrlSync.tsx';
import { StatusStrip } from './StatusStrip';

/** Shell layout per plan 11a: nav rail | (scope bar / content / status strip).
 * The inspector panel mounts inside workspace content (archetype-owned) so
 * its width interacts with the content grid, not the shell. */
export function Shell() {
  const [paletteOpen, setPaletteOpen] = useState(false);
  usePaletteHotkey(setPaletteOpen);
  return (
    <div className="flex h-dvh w-full overflow-hidden bg-surface-0 text-text-primary">
      <a
        href="#td-main"
        className="sr-only focus:not-sr-only focus:absolute focus:left-2 focus:top-2 focus:z-50 focus:rounded-[var(--radius-standard)] focus:bg-surface-3 focus:px-3 focus:py-2"
      >
        Skip to content
      </a>
      <NavRail />
      <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
      <div className="flex min-w-0 flex-1 flex-col">
        <ScopeUrlSync />
        <ScopeBar onOpenPalette={() => setPaletteOpen(true)} />
        {/* Named because it is also the page's scroll container: a workspace
          * whose content outruns the viewport scrolls HERE rather than losing
          * the overflow, and Plan 11 licenses internal scrolling for labelled
          * regions only. */}
        <main id="td-main" aria-label="Active workspace" className="min-h-0 flex-1 overflow-auto">
          <Outlet />
        </main>
        <StatusStrip />
      </div>
    </div>
  );
}
