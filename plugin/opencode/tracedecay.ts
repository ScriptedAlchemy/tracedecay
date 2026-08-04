import type { Hooks, Plugin } from "@opencode-ai/plugin"

const TRACEDECAY_BIN = "__TRACEDECAY_BIN__"
const HOOK_DISPATCH_BUDGET_MS = 15
const MAX_GUIDANCE_BYTES = 8 * 1024

export async function dispatch(
  command: string,
  payload: unknown,
  executable = TRACEDECAY_BIN,
): Promise<string | undefined> {
  let process: ReturnType<typeof Bun.spawn>
  try {
    process = Bun.spawn([executable, command], {
      stdin: new Blob([JSON.stringify(payload)]),
      stdout: "pipe",
      stderr: "ignore",
    })
  } catch {
    return undefined
  }

  let timeout: ReturnType<typeof setTimeout> | undefined
  const timedOut = new Promise<undefined>((resolve) => {
    timeout = setTimeout(() => {
      stop(process)
      resolve(undefined)
    }, HOOK_DISPATCH_BUDGET_MS)
  })

  try {
    return await Promise.race([completedGuidance(process), timedOut])
  } finally {
    if (timeout !== undefined) clearTimeout(timeout)
  }
}

/// Schedule the bounded daemon child after the host callback has acknowledged
/// its event. OpenCode keeps the plugin runtime alive between callbacks, so
/// this preserves durable hook delivery without charging process startup to a
/// hook deadline.
export function dispatchAfterAck(
  command: string,
  payload: unknown,
  executable = TRACEDECAY_BIN,
): void {
  setTimeout(() => {
    void dispatch(command, payload, executable).catch(() => undefined)
  }, 0)
}

async function completedGuidance(process: ReturnType<typeof Bun.spawn>): Promise<string | undefined> {
  const [status, output] = await Promise.all([
    process.exited,
    readBoundedGuidance(process.stdout, process),
  ])
  if (status !== 0 || output === undefined) return undefined
  const guidance = output.trim()
  return guidance.length > 0 ? guidance : undefined
}

async function readBoundedGuidance(
  stdout: ReadableStream<Uint8Array>,
  process: ReturnType<typeof Bun.spawn>,
): Promise<string | undefined> {
  const reader = stdout.getReader()
  const decoder = new TextDecoder()
  const chunks: string[] = []
  let byteLength = 0

  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      byteLength += value.byteLength
      if (byteLength > MAX_GUIDANCE_BYTES) {
        stop(process)
        await reader.cancel()
        return undefined
      }
      chunks.push(decoder.decode(value, { stream: true }))
    }
  } catch {
    return undefined
  } finally {
    reader.releaseLock()
  }

  chunks.push(decoder.decode())
  return chunks.join("")
}

function stop(process: ReturnType<typeof Bun.spawn>): void {
  try {
    process.kill()
  } catch {
    // The child can win the race and exit before the hook deadline fires.
  }
}

export const TraceDecayPlugin: Plugin = async () => {
  return {
    event: ({ event }) => {
      if (
        event.type === "file.edited" ||
        event.type === "lsp.updated" ||
        event.type === "session.idle" ||
        (event.type === "session.status" && event.properties.status.type === "idle")
      ) {
        dispatchAfterAck("hook-opencode-event", event)
      }
    },
    "tool.execute.after": (
      input: Parameters<NonNullable<Hooks["tool.execute.after"]>>[0],
      output: Parameters<NonNullable<Hooks["tool.execute.after"]>>[1],
    ) => {
      dispatchAfterAck("hook-opencode-tool-after", { input, output })
    },
  }
}
