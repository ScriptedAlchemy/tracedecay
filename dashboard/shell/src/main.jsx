/**
 * Standalone dashboard shell for `tokensave dashboard`.
 *
 * Mirrors what the Hermes dashboard SPA does for plugin tabs:
 *   1. expose a plugin SDK + registry on window (BEFORE loading bundles),
 *   2. fetch the plugin manifest list from /api/dashboard/plugins,
 *   3. inject each plugin's CSS <link> and JS <script>,
 *   4. render registered components behind a tab bar.
 *
 * The plugin bundles themselves are byte-compatible with the Hermes-hosted
 * variants — they register via window.__HERMES_PLUGINS__.register(name, C).
 */

import React, { useEffect, useState, useCallback, useRef, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";
import { buildSDK, fetchJSON, cn } from "./sdk.jsx";

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

const registered = new Map();
const listeners = new Set();
let registrySnapshot = 0;

function notify() {
  registrySnapshot += 1;
  for (const fn of listeners) {
    try {
      fn();
    } catch {
      /* ignore */
    }
  }
}

window.__HERMES_PLUGIN_SDK__ = buildSDK();
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
    () => registrySnapshot,
  );
}

// ---------------------------------------------------------------------------
// Theme helpers (dark default, light variant, localStorage + prefers-color-scheme)
// ---------------------------------------------------------------------------

function getInitialTheme() {
  try {
    const stored = localStorage.getItem("ts-theme");
    if (stored === "light" || stored === "dark") return stored;
  } catch {
    /* storage unavailable */
  }
  return window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

function applyTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme);
}

// Apply immediately (before React render) to avoid flash.
applyTheme(getInitialTheme());

// ---------------------------------------------------------------------------
// Tab ↔ URL state (?tab=<plugin>) so tabs deep-link and back/forward work
// ---------------------------------------------------------------------------

function tabFromUrl() {
  try {
    return new URLSearchParams(window.location.search).get("tab") || "";
  } catch {
    return "";
  }
}

function writeTabToUrl(name, { push = true } = {}) {
  try {
    const url = new URL(window.location);
    if (url.searchParams.get("tab") === name) return;
    url.searchParams.set("tab", name);
    if (push) window.history.pushState({ tab: name }, "", url);
    else window.history.replaceState({ tab: name }, "", url);
  } catch {
    /* URL state is best-effort */
  }
}

// ---------------------------------------------------------------------------
// Error boundary — wraps each plugin tab so one crash can't blank the shell
// ---------------------------------------------------------------------------

class ErrorBoundary extends React.Component {
  constructor(props) {
    super(props);
    this.state = { error: null };
    this.handleRetry = this.handleRetry.bind(this);
  }

  static getDerivedStateFromError(error) {
    return { error };
  }

  handleRetry() {
    this.setState({ error: null });
  }

  render() {
    if (this.state.error) {
      return (
        <div className="ts-error-boundary">
          <div className="ts-error-boundary-icon">⚠</div>
          <div className="ts-error-boundary-title">Plugin crashed</div>
          <div className="ts-error-boundary-msg">{String(this.state.error)}</div>
          <button className="ts-button ts-button-outline ts-button-sm" onClick={this.handleRetry}>
            Retry
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

// ---------------------------------------------------------------------------
// Plugin asset loading
// ---------------------------------------------------------------------------

function injectPluginAssets(manifest) {
  if (manifest.css) {
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = `/dashboard-plugins/${manifest.name}/${manifest.css}`;
    document.head.appendChild(link);
  }
  const script = document.createElement("script");
  script.src = `/dashboard-plugins/${manifest.name}/${manifest.entry || "dist/index.js"}`;
  script.async = false;
  document.body.appendChild(script);
}

// ---------------------------------------------------------------------------
// Favicon (SVG data URL — trivially embeddable, no extra file)
// ---------------------------------------------------------------------------

(function injectFavicon() {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
    <rect width="32" height="32" rx="7" fill="#07100f"/>
    <text x="16" y="23" font-size="19" text-anchor="middle" fill="#75f4d2" font-family="monospace">&#x25F3;</text>
  </svg>`;
  const link = document.createElement("link");
  link.rel = "icon";
  link.type = "image/svg+xml";
  link.href = `data:image/svg+xml,${encodeURIComponent(svg)}`;
  document.head.appendChild(link);
})();

// ---------------------------------------------------------------------------
// Connection polling interval (ms)
// ---------------------------------------------------------------------------

const POLL_INTERVAL_MS = 15_000;

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

function App() {
  const [plugins, setPlugins] = useState([]);
  const [capabilities, setCapabilities] = useState(null);
  const [active, setActive] = useState("");
  // Tabs that have been activated at least once; their panels stay mounted
  // (hidden) afterwards so in-progress exploration survives tab switches.
  const [visited, setVisited] = useState(() => new Set());
  const [loadError, setLoadError] = useState("");
  const [connState, setConnState] = useState("connecting"); // "connecting" | "ok" | "error"
  const [lastRefresh, setLastRefresh] = useState(null);
  const [theme, setTheme] = useState(getInitialTheme);
  const pluginsRef = useRef([]);
  useRegistryVersion();

  /** Single entry point for tab changes; keeps the URL in sync. */
  const selectTab = useCallback((name, { push = true, writeUrl = true } = {}) => {
    setActive(name);
    setVisited((prev) => {
      if (prev.has(name)) return prev;
      const next = new Set(prev);
      next.add(name);
      return next;
    });
    if (writeUrl) writeTabToUrl(name, { push });
  }, []);

  // Browser back/forward: restore the tab encoded in the URL. History entries
  // without a (known) tab — e.g. ones created before this app pushed any state
  // — fall back to the first plugin so Back never leaves the URL and UI
  // disagreeing about the active tab.
  useEffect(() => {
    function onPopState() {
      const name = tabFromUrl();
      const list = pluginsRef.current;
      const target = list.some((p) => p.name === name) ? name : list[0]?.name;
      if (target) selectTab(target, { writeUrl: false });
    }
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, [selectTab]);

  // Keep ref in sync so keyboard handler always sees current plugins.
  useEffect(() => {
    pluginsRef.current = plugins;
  }, [plugins]);

  // Apply theme to <html> and persist.
  useEffect(() => {
    applyTheme(theme);
    try {
      localStorage.setItem("ts-theme", theme);
    } catch {
      /* storage unavailable */
    }
  }, [theme]);

  const toggleTheme = useCallback(() => {
    setTheme((t) => (t === "dark" ? "light" : "dark"));
  }, []);

  // Fetch capabilities, update SDK, and return the payload (or null on failure).
  const fetchCapabilities = useCallback(async () => {
    try {
      const caps = await fetchJSON("/api/capabilities");
      setCapabilities(caps);
      setConnState("ok");
      setLastRefresh(Date.now());
      // Expose fetched capabilities on the SDK so plugin tabs can feature-gate.
      window.__HERMES_PLUGIN_SDK__.capabilities = caps;
      return caps;
    } catch {
      setConnState("error");
      return null;
    }
  }, []);

  // Initial load: plugin list + capabilities in parallel.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [list] = await Promise.all([
          fetchJSON("/api/dashboard/plugins"),
          fetchCapabilities(),
        ]);
        if (cancelled) return;
        setPlugins(list);
        if (list.length > 0) {
          const fromUrl = tabFromUrl();
          const initial = list.some((p) => p.name === fromUrl) ? fromUrl : list[0].name;
          // Always write the resolved tab into the URL (replaceState) so the
          // initial entry round-trips through back/forward like any other tab.
          selectTab(initial, { push: false });
        }
        for (const manifest of list) injectPluginAssets(manifest);
      } catch (err) {
        if (!cancelled) {
          setLoadError(String(err));
          setConnState("error");
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Poll capabilities for connection health.
  useEffect(() => {
    const id = setInterval(fetchCapabilities, POLL_INTERVAL_MS);
    return () => clearInterval(id);
  }, [fetchCapabilities]);

  // Keyboard shortcuts: digits 1–9 switch tabs. Skipped for modified
  // keystrokes (browser/OS shortcuts) and when focus sits in a widget that
  // owns its keyboard interaction — form fields, and svg/application/listbox
  // surfaces like the semantic map, histogram brush, graph nodes, and fact
  // lists, where a stray digit must not yank the user to another tab.
  useEffect(() => {
    function ownsKeyboard(target) {
      if (!target) return false;
      if (target.isContentEditable) return true;
      if (typeof target.closest !== "function") return false;
      return !!target.closest(
        'input, textarea, select, svg, [contenteditable="true"], [role="application"], [role="listbox"]',
      );
    }
    function onKeyDown(e) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (ownsKeyboard(e.target)) return;
      const idx = parseInt(e.key, 10);
      if (idx >= 1 && idx <= 9) {
        const target = pluginsRef.current[idx - 1];
        if (target) selectTab(target.name);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [selectTab]);

  // ARIA tabs pattern: roving tabindex + arrow-key navigation on the tablist.
  const tabRefs = useRef(new Map());

  const onTablistKeyDown = useCallback((e) => {
    const list = pluginsRef.current;
    if (list.length === 0) return;
    const currentIdx = list.findIndex((p) => p.name === document.activeElement?.dataset?.tabName);
    let nextIdx = null;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") {
      nextIdx = currentIdx < 0 ? 0 : (currentIdx + 1) % list.length;
    } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
      nextIdx = currentIdx < 0 ? list.length - 1 : (currentIdx - 1 + list.length) % list.length;
    } else if (e.key === "Home") {
      nextIdx = 0;
    } else if (e.key === "End") {
      nextIdx = list.length - 1;
    }
    if (nextIdx === null) return;
    e.preventDefault();
    const next = list[nextIdx];
    selectTab(next.name);
    tabRefs.current.get(next.name)?.focus();
  }, [selectTab]);

  // Document title reflecting the active tab.
  useEffect(() => {
    const manifest = plugins.find((p) => p.name === active);
    const label = manifest?.label || active;
    document.title = label ? `${label} — tokensave` : "tokensave dashboard";
  }, [active, plugins]);

  const activeManifest = plugins.find((p) => p.name === active);
  const ActiveComponent = registered.get(active);
  const projectName =
    capabilities?.project_root?.split("/").filter(Boolean).pop() ?? capabilities?.project_root;

  const connLabel =
    connState === "ok"
      ? `Connected · refreshed ${lastRefresh ? new Date(lastRefresh).toLocaleTimeString() : ""}`
      : connState === "error"
        ? "Server disconnected — click to retry"
        : "Connecting…";

  return (
    <div className="ts-shell">
      <header className="ts-shell-header">
        <div className="ts-shell-brand">
          <span className="ts-shell-logo" aria-hidden="true">◳</span>
          <h1 className="ts-shell-title">tokensave</h1>
          {projectName && (
            <span className="ts-shell-project" title={capabilities?.project_root}>
              {projectName}
            </span>
          )}
        </div>

        <div
          className="ts-shell-tabs"
          role="tablist"
          aria-label="Plugin tabs"
          onKeyDown={onTablistKeyDown}
        >
          {plugins.map((p, i) => (
            <button
              key={p.name}
              ref={(el) => {
                if (el) tabRefs.current.set(p.name, el);
                else tabRefs.current.delete(p.name);
              }}
              data-tab-name={p.name}
              id={`ts-tab-${p.name}`}
              role="tab"
              aria-selected={p.name === active}
              aria-controls={`ts-tabpanel-${p.name}`}
              tabIndex={p.name === active ? 0 : -1}
              className={cn("ts-shell-tab", p.name === active && "ts-shell-tab-active")}
              onClick={() => selectTab(p.name)}
              title={`${p.label || p.name} (${i + 1})`}
            >
              {p.label || p.name}
            </button>
          ))}
        </div>

        <div className="ts-shell-controls">
          <button
            className={cn("ts-conn-indicator", `ts-conn-indicator-${connState}`)}
            title={connLabel}
            aria-label={connLabel}
            onClick={connState === "error" ? fetchCapabilities : undefined}
          >
            <span className="ts-conn-dot" />
            {lastRefresh && (
              <span className="ts-conn-time">{new Date(lastRefresh).toLocaleTimeString()}</span>
            )}
          </button>

          <button
            className="ts-theme-toggle"
            onClick={toggleTheme}
            aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} theme`}
            title={`Switch to ${theme === "dark" ? "light" : "dark"} theme`}
          >
            {theme === "dark" ? "☀" : "☾"}
          </button>
        </div>
      </header>

      {connState === "error" && !loadError && (
        <div className="ts-disconnected-banner" role="alert">
          <span className="ts-disconnected-banner-msg">
            Server disconnected — retrying automatically
          </span>
          <button
            className="ts-button ts-button-outline ts-button-xs"
            onClick={fetchCapabilities}
          >
            Retry now
          </button>
        </div>
      )}

      <main className="ts-shell-main">
        {loadError && (
          <div className="ts-shell-error" role="alert">
            Failed to load dashboard: {loadError}
          </div>
        )}
        {!loadError && !activeManifest && (
          <div className="ts-shell-loading" role="status" aria-live="polite">
            Loading plugins…
          </div>
        )}
        {activeManifest && !ActiveComponent && (
          <div className="ts-shell-loading" role="status" aria-live="polite">
            Loading {activeManifest.label || active}…
          </div>
        )}
        {/* Visited panels stay mounted (hidden) so tab switches don't reset
            in-progress exploration like the code-graph canvas. */}
        {plugins
          .filter((p) => visited.has(p.name) && registered.get(p.name))
          .map((p) => {
            const Component = registered.get(p.name);
            return (
              <div
                key={p.name}
                role="tabpanel"
                id={`ts-tabpanel-${p.name}`}
                aria-labelledby={`ts-tab-${p.name}`}
                className="ts-shell-tabpanel"
                hidden={p.name !== active}
              >
                <ErrorBoundary key={p.name}>
                  <Component />
                </ErrorBoundary>
              </div>
            );
          })}
      </main>
    </div>
  );
}

createRoot(document.getElementById("root")).render(<App />);
