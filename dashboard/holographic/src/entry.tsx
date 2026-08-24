/**
 * Plugin entry — registers the holographic-memory page with the host dashboard.
 *
 * The host loads this IIFE via a <script> tag (see `web/src/plugins/usePlugins.ts`)
 * after it has called `exposePluginSDK()`, so `window.__HERMES_PLUGIN_SDK__` and
 * `window.__HERMES_PLUGINS__` are present by the time this runs.
 */

import HolographicMemoryPage from "./HolographicMemoryPage";

interface PluginRegistry {
  register: (name: string, component: unknown) => void;
}

const registry: PluginRegistry | null =
  (typeof window !== "undefined" &&
    (window as unknown as { __HERMES_PLUGINS__?: PluginRegistry })
      .__HERMES_PLUGINS__) ||
  null;

const sdk =
  typeof window !== "undefined" &&
  (window as unknown as { __HERMES_PLUGIN_SDK__?: unknown })
    .__HERMES_PLUGIN_SDK__;

if (sdk && registry && typeof registry.register === "function") {
  registry.register("holographic", HolographicMemoryPage);
}
