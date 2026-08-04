import type { Hooks, Plugin } from "@opencode-ai/plugin"

const TRACEDECAY_BIN = "__TRACEDECAY_BIN__"
const HOOK_DISPATCH_BUDGET_MS = 25
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

  let timeoutId: ReturnType<typeof setTimeout> | undefined
  const timedOut = new Promise<undefined>((resolve) => {
    timeoutId = setTimeout(() => {
      stop(process)
      resolve(undefined)
    }, HOOK_DISPATCH_BUDGET_MS)
  })

  try {
    return await Promise.race([completedGuidance(process), timedOut])
  } finally {
    if (timeoutId !== undefined) clearTimeout(timeoutId)
  }
}

export function dispatchAfterAck(
  command: string,
  payload: unknown,
  executable = TRACEDECAY_BIN,
): void {
  setTimeout(() => {
    void dispatch(command, payload, executable).catch(() => undefined)
  }, 0)
}

async function completedGuidance(process: Bun.Subprocess): Promise<string | undefined> {
  const [status, guidance] = await Promise.all([process.exited, readGuidance(process)])
  return status === 0 ? guidance : undefined
}

async function readGuidance(process: Bun.Subprocess): Promise<string | undefined> {
  const reader = process.stdout.getReader()
  const chunks: Uint8Array[] = []
  let byteLength = 0

  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break

      byteLength += value.byteLength
      if (byteLength > MAX_GUIDANCE_BYTES) {
        stop(process)
        return undefined
      }
      chunks.push(value)
    }
  } catch {
    return undefined
  } finally {
    void reader.cancel().catch(() => undefined)
  }

  const guidance = new TextDecoder().decode(concatenate(chunks, byteLength)).trim()
  return guidance.length > 0 ? guidance : undefined
}

function concatenate(chunks: Uint8Array[], byteLength: number): Uint8Array {
  const output = new Uint8Array(byteLength)
  let offset = 0
  for (const chunk of chunks) {
    output.set(chunk, offset)
    offset += chunk.byteLength
  }
  return output
}

function stop(process: Bun.Subprocess): void {
  try {
    process.kill()
  } catch {
    // The child may already have completed between the budget check and kill.
  }
}

export const TraceDecayPlugin: Plugin = async () => ({
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
})
