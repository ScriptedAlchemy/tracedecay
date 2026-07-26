import type { Hooks, Plugin } from "@opencode-ai/plugin"

const TRACEDECAY_BIN = "__TRACEDECAY_BIN__"

async function dispatch(command: string, payload: unknown): Promise<string | undefined> {
  const process = Bun.spawn([TRACEDECAY_BIN, command], {
    stdin: new Blob([JSON.stringify(payload)]),
    stdout: "pipe",
    stderr: "ignore",
  })
  const [status, stdout] = await Promise.all([
    process.exited,
    new Response(process.stdout).text(),
  ])
  if (status !== 0) return undefined
  const guidance = stdout.trim()
  return guidance.length > 0 ? guidance : undefined
}

export const TraceDecayPlugin: Plugin = async ({ client }) => {
  const deliver = async (guidance: string | undefined): Promise<void> => {
    if (!guidance) return
    await client.tui.showToast({
      body: { message: guidance, variant: "info" },
    })
  }

  return {
    event: async ({ event }) => {
      if (
        event.type === "file.edited" ||
        event.type === "lsp.updated" ||
        event.type === "session.idle" ||
        (event.type === "session.status" && event.properties.status.type === "idle")
      ) {
        await deliver(await dispatch("hook-opencode-event", event))
      }
    },
    "tool.execute.after": async (
      input: Parameters<NonNullable<Hooks["tool.execute.after"]>>[0],
      output: Parameters<NonNullable<Hooks["tool.execute.after"]>>[1],
    ) => {
      await deliver(await dispatch("hook-opencode-tool-after", { input, output }))
    },
  }
}
