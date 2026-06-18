/**
 * Dev entry for the tracedecay dashboard frontend.
 *
 * This mirrors what the prod shell (dashboard/shell/src/main.jsx) does, but for
 * the dev server: it builds the plugin SDK on window, installs the plugin
 * registry, then dynamically imports each plugin entry so it can register.
 * A minimal tab shell renders whatever registered.
 *
 * MODULE-LOAD ORDER (the guarantee that makes this work):
 *
 *   1. Static imports (CSS + buildSDK) evaluate first (ESM hoisting). buildSDK
 *      is only a function definition here — no SDK read happens yet.
 *   2. The module body runs SYNCHRONOUSLY before any dynamic import():
 *        window.__HERMES_PLUGIN_SDK__ = buildSDK();   // React + hooks +
 *                                                     // components + utils +
 *                                                     // fetchJSON + capabilities
 *        window.__HERMES_PLUGINS__   = { register, registerSlot };
 *      So by the time ANY plugin entry's module code runs, the SDK and the
 *      registry are fully populated on window.
 *   3. loadPlugins() is fired (async). Its dynamic import() calls resolve on a
 *      later tick; each imported entry reads window.__HERMES_PLUGIN_SDK__ /
 *      window.__HERMES_PLUGINS__ (already set) and calls register().
 *   4. createRoot(...).render(<App/>) runs synchronously after the kick-off.
 *      App subscribes to the registry (useRegistryVersion), so registrations
 *      arriving from step 3 re-render the tab bar live.
 *
 * REACT IN DEV (divergence from prod — see run.mjs / return note):
 * The dev server does NOT alias `react` onto a window-SDK shim. In a single
 * Rsbuild bundle every module already shares one real React instance, and
 * react-dom/client (used below for createRoot) needs the real `react` module
 * with its internal symbols. We still expose real React + hooks on
 * window.__HERMES_PLUGIN_SDK__ so plugin code that reads the SDK (e.g.
 * dashboard/lib/sdk.ts, lcm/src/entry.tsx) behaves exactly like in prod.
 */

import { useState, useEffect, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";
import { buildSDK } from "../shell/src/sdk.jsx";

// Shell theme tokens + SDK component classes (.ts-*). Plugin CSS layers below
// consume the same CSS variables.
import "../shell/src/styles.css";
import "../graph/src/styles.css";
import "../savings/src/styles.css";
import "../lcm/src/styles.css";
// Holographic styles begin with `@import "tailwindcss"` (Tailwind v4). In this
// execution environment both Tailwind-v4 integrations Rsbuild documents make
// createRsbuild() segfault, so the dev server (dev/run.mjs) ships NO Tailwind
// pipeline — this CSS is passed through uncompiled and holographic renders
// unstyled in dev. The prod build (`npm run build`) is the source of truth for
// holographic's Tailwind styles. See run.mjs for the full divergence note.
import "../holographic/src/styles.css";

// ---------------------------------------------------------------------------
// SDK + plugin registry — populated BEFORE plugin entries are imported.
// ---------------------------------------------------------------------------

window.__HERMES_PLUGIN_SDK__ = buildSDK();

const registered = new Map();
const listeners = new Set();
let registryVersion = 0;

function notify() {
  registryVersion += 1;
  for (const fn of listeners) {
    try {
      fn();
    } catch {
      /* listener errors must not break registration */
    }
  }
}

window.__HERMES_PLUGINS__ = {
  register(name, component) {
    registered.set(name, component);
    notify();
  },
  registerSlot() {},
};

function useRegistryVersion() {
  return useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    () => registryVersion,
    () => registryVersion,
  );
}

// Apply the dark theme early so the first paint matches the shell.
try {
  document.documentElement.setAttribute("data-theme", "dark");
} catch {
  /* non-browser */
}

// ---------------------------------------------------------------------------
// Plugin discovery + registration (dynamic, fault-tolerant).
//
// Each entry is imported AFTER the SDK/registry exist on window. Missing or
// erroring entries are warned and skipped so the dev server stays usable.
// ---------------------------------------------------------------------------

const PLUGIN_ENTRIES = [
  { name: "holographic", spec: "../holographic/src/entry.tsx" },
  { name: "graph", spec: "../graph/src/entry.tsx" },
  { name: "savings", spec: "../savings/src/entry.tsx" },
  { name: "hermes-lcm", spec: "../lcm/src/entry.tsx" },
];

async function loadPlugins() {
  await Promise.all(
    PLUGIN_ENTRIES.map(async (p) => {
      try {
        await import(/* @vite-ignore */ p.spec);
        return;
      } catch (err) {
        // Rsbuild leaves a stack in `err`; keep the console line scannable.
        console.warn(`[tracedecay dev] failed to load "${p.spec}":`, err);
      }
      console.warn(`[tracedecay dev] plugin "${p.name}" has no loadable entry — skipping.`);
    }),
  );
}

// Fire-and-forget: registrations update the UI live via useRegistryVersion.
loadPlugins();

// ---------------------------------------------------------------------------
// Minimal dev shell: a tab bar over the registered plugin components.
// Intentionally smaller than prod's App (no URL sync, polling, or asset
// injection) — plugins are imported by the dev bundle, not fetched.
// ---------------------------------------------------------------------------

const PLUGIN_LABELS = {
  holographic: "Holographic Memory",
  graph: "Code Graph",
  savings: "Savings & Cost",
  "hermes-lcm": "LCM",
};

function App() {
  useRegistryVersion();
  const names = Array.from(registered.keys());
  const [active, setActive] = useState("");

  useEffect(() => {
    if ((!active || !registered.has(active)) && names.length > 0) {
      setActive(names[0]);
    }
  }, [names, active]);

  const tabs = names.map((n) => ({ name: n, label: PLUGIN_LABELS[n] || n }));
  const Active = active ? registered.get(active) : null;

  return (
    <div className="ts-shell">
      <header className="ts-shell-header">
        <div className="ts-shell-brand">
          <span className="ts-shell-logo" aria-hidden="true">
            ◳
          </span>
          <h1 className="ts-shell-title">tracedecay · dev</h1>
        </div>
        <div className="ts-shell-tabs" role="tablist" aria-label="Plugin tabs (dev)">
          {tabs.map((t) => (
            <button
              key={t.name}
              type="button"
              role="tab"
              aria-selected={t.name === active}
              tabIndex={t.name === active ? 0 : -1}
              className={`ts-shell-tab${t.name === active ? " ts-shell-tab-active" : ""}`}
              onClick={() => setActive(t.name)}
            >
              {t.label}
            </button>
          ))}
          {tabs.length === 0 && (
            <span style={{ padding: "0.5rem 1rem", color: "var(--ts-text-3)" }}>
              No plugins registered…
            </span>
          )}
        </div>
      </header>
      <main className="ts-shell-main">
        {Active ? (
          <Active />
        ) : (
          <div className="ts-shell-loading" role="status" aria-live="polite">
            Waiting for plugins to register…
          </div>
        )}
      </main>
    </div>
  );
}

createRoot(document.getElementById("root")).render(<App />);
