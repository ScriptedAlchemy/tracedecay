import type { Plugin } from "@opencode/plugin"

const TRACEDECAY_BIN = "__TRACEDECAY_BIN__"

export const TraceDecayMcpPlugin: Plugin.Plugin = {
  id: "tracedecay-mcp",
  async setup(ctx) {
    await ctx.mcp.transform((editor) => {
      editor.set("tracedecay", {
        type: "local",
        command: [TRACEDECAY_BIN, "serve"],
      })
    })
  },
}

export default TraceDecayMcpPlugin
