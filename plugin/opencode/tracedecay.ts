import type { Hooks, Plugin } from "@opencode-ai/plugin"

const TRACEDECAY_BIN = "__TRACEDECAY_BIN__"
const MAX_GUIDANCE_BYTES = 8 * 1024

export async function dispatch(
  command: string,
  payload: unknown,
  executable = TRACEDECAY_BIN,
): Promise<string | undefined> {
  let process: Bun.Subprocess
  try {
    process = Bun.spawn([executable, command], {
      stdin: new Blob([JSON.stringify(payload)]),
      stdout: "pipe",
      stderr: "ignore",
    })
  } catch {
    return undefined
  }
  const [status, guidance] = await Promise.all([
    process.exited,
    readBoundedGuidance(process.stdout),
  ])
  return status === 0 ? guidance : undefined
}

export function dispatchAfterAck(
  command: string,
  payload: unknown,
  deliver: (guidance: string | undefined) => Promise<void>,
  executable = TRACEDECAY_BIN,
): void {
  // `dispatch` spawns synchronously before its first await, so the native
  // hook process owns the event before OpenCode receives this callback's ack.
  void dispatch(command, payload, executable)
    .then(deliver)
    .catch(() => undefined)
}

async function readBoundedGuidance(
  stdout: ReadableStream<Uint8Array>,
): Promise<string | undefined> {
  const reader = stdout.getReader()
  const chunks: Uint8Array[] = []
  let retainedBytes = 0
  let oversized = false
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      if (!oversized && retainedBytes + value.byteLength <= MAX_GUIDANCE_BYTES) {
        chunks.push(value)
        retainedBytes += value.byteLength
      } else {
        oversized = true
        chunks.length = 0
      }
    }
  } catch {
    return undefined
  }
  if (oversized) return undefined
  const output = new Uint8Array(retainedBytes)
  let offset = 0
  for (const chunk of chunks) {
    output.set(chunk, offset)
    offset += chunk.byteLength
  }
  const guidance = new TextDecoder().decode(output).trim()
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
    event: ({ event }) => {
      if (
        event.type === "file.edited" ||
        event.type === "lsp.updated" ||
        event.type === "session.idle" ||
        (event.type === "session.status" && event.properties.status.type === "idle")
      ) {
        dispatchAfterAck("hook-opencode-event", event, deliver)
      }
    },
    "tool.execute.after": (
      input: Parameters<NonNullable<Hooks["tool.execute.after"]>>[0],
      output: Parameters<NonNullable<Hooks["tool.execute.after"]>>[1],
    ) => {
      dispatchAfterAck("hook-opencode-tool-after", { input, output }, deliver)
    },
  }
}

export default TraceDecayPlugin
