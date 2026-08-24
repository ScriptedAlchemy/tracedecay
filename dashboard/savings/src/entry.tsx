import SavingsExplorer from "./SavingsExplorer";

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
  registry.register("savings", SavingsExplorer);
}
