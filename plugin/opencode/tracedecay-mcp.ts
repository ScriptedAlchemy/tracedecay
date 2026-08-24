import type { Plugin } from "@opencode-ai/plugin"

const TRACEDECAY_BIN = "__TRACEDECAY_BIN__"

export const TraceDecayMcpPlugin: Plugin = async () => ({
  config: async (config) => {
    config.mcp ??= {}
    config.mcp.tracedecay = {
      type: "local",
      command: [TRACEDECAY_BIN, "serve"],
    }
  },
})

export default TraceDecayMcpPlugin
