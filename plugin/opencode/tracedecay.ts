import type { Plugin } from "@opencode/plugin"

const TRACEDECAY_BIN = "__TRACEDECAY_BIN__"
const MAX_GUIDANCE_BYTES = 8 * 1024
const MAX_PENDING_GUIDANCE_PER_SESSION = 16
const MAX_PENDING_GUIDANCE_SESSIONS = 128

// Durable session boundaries on OpenCode's public event stream. The
// deprecated `session.idle` / `session.status` pair is not emitted for V2
// executions, and no stream event reports a tool edit; edits arrive only
// through the `execute.after` tool hook.
export const SESSION_BOUNDARY_EVENT_TYPES: ReadonlySet<string> = new Set([
  "session.execution.succeeded",
  "session.execution.failed",
  "session.execution.interrupted",
])

export interface StreamEvent {
  readonly type: string
  readonly location?: { readonly directory: string }
  readonly data?: unknown
}

export function sessionOf(event: { readonly data?: unknown }): string | undefined {
  const data = event.data as { sessionID?: unknown } | undefined
  return typeof data?.sessionID === "string" ? data.sessionID : undefined
}

// A shared OpenCode server loads one plugin instance per location but
// publishes every location's events to each of them, and the execution
// boundary events carry no `location`. Sessions are attributed from the
// located events that precede a boundary (`session.created`, tool and step
// events) so a boundary is dispatched once, by the instance whose location
// owns the session.
export class SessionLocations {
  private readonly owned = new Set<string>()
  private readonly foreign = new Set<string>()

  constructor(private readonly directory: string) {}

  observe(event: StreamEvent): void {
    const sessionID = sessionOf(event)
    if (!sessionID) return
    if (event.type === "session.deleted") {
      this.owned.delete(sessionID)
      this.foreign.delete(sessionID)
      return
    }
    if (!event.location) return
    if (event.location.directory === this.directory) {
      this.owned.add(sessionID)
    } else {
      this.foreign.add(sessionID)
    }
  }

  isSessionBoundary(event: StreamEvent): boolean {
    const sessionID = sessionOf(event)
    return (
      SESSION_BOUNDARY_EVENT_TYPES.has(event.type) &&
      sessionID !== undefined &&
      this.owned.has(sessionID)
    )
  }
}

// Daemon guidance is model-facing context. V2 server plugins have no client
// UI channel, so guidance waits for the owning session's next model request
// and is injected there as system context.
export class PendingGuidance {
  private readonly bySession = new Map<string, string[]>()

  deliveryFor(sessionID: string): (guidance: string | undefined) => void {
    let queue = this.bySession.get(sessionID)
    if (!queue) {
      queue = []
    }
    this.bySession.delete(sessionID)
    this.bySession.set(sessionID, queue)
    if (this.bySession.size > MAX_PENDING_GUIDANCE_SESSIONS) {
      let evicted = this.bySession.keys().next().value
      for (const [id, guidance] of this.bySession) {
        if (id !== sessionID && guidance.length === 0) {
          evicted = id
          break
        }
      }
      if (evicted !== undefined) this.bySession.delete(evicted)
    }
    const captured = queue
    return (guidance) => {
      if (!guidance || this.bySession.get(sessionID) !== captured) return
      if (captured.length >= MAX_PENDING_GUIDANCE_PER_SESSION) captured.shift()
      captured.push(guidance)
    }
  }

  delete(sessionID: string): void {
    this.bySession.delete(sessionID)
  }

  clear(): void {
    this.bySession.clear()
  }

  drain(sessionID: string): string[] {
    return this.bySession.get(sessionID)?.splice(0) ?? []
  }
}

export async function dispatch(
  command: string,
  payload: unknown,
  executable = TRACEDECAY_BIN,
  cwd?: string,
): Promise<string | undefined> {
  let process: Bun.Subprocess<Blob, "pipe", "ignore">
  try {
    process = Bun.spawn([executable, command], {
      cwd,
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
  deliver: (guidance: string | undefined) => void,
  executable = TRACEDECAY_BIN,
  cwd?: string,
): void {
  // `dispatch` spawns synchronously before its first await, so the native
  // hook process owns the event before OpenCode receives this callback's ack.
  void dispatch(command, payload, executable, cwd)
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

export const TraceDecayPlugin: Plugin.Plugin = {
  id: "tracedecay-hooks",
  async setup(ctx) {
    const cwd = ctx.location.directory
    const sessions = new SessionLocations(cwd)
    const pending = new PendingGuidance()

    await ctx.session.hook("context", (event) => {
      for (const text of pending.drain(event.sessionID)) {
        event.system.push({ type: "text", text })
      }
    })

    await ctx.tool.hook("execute.after", (event) => {
      dispatchAfterAck(
        "hook-opencode-tool-after",
        event,
        pending.deliveryFor(event.sessionID),
        TRACEDECAY_BIN,
        cwd,
      )
    })

    const controller = new AbortController()
    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
          sessions.observe(event)
          const sessionID = sessionOf(event)
          if (event.type === "session.deleted" && sessionID) pending.delete(sessionID)
          if (!sessions.isSessionBoundary(event)) continue
          if (!sessionID) continue
          dispatchAfterAck(
            "hook-opencode-event",
            event,
            pending.deliveryFor(sessionID),
            TRACEDECAY_BIN,
            cwd,
          )
        }
      } catch {
        // The subscription ends with the plugin scope; the abort below is the
        // only expected termination.
      }
    })()

    return () => {
      controller.abort()
      pending.clear()
    }
  },
}

export default TraceDecayPlugin
