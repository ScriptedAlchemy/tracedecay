import { expect, test } from "bun:test"

import {
  type CatalogToolSchema,
  DAEMON_UNREACHABLE_EXIT_CODE,
  dispatchLifecycle,
  lifecyclePayload,
  parseToolSchemas,
  type RunOptions,
  type RunResult,
  registerCatalogTools,
  runCatalogTool,
  runCli,
} from "./index"

const readOnly: CatalogToolSchema = {
  name: "tracedecay_find_exact_symbol",
  description: "Exact-name symbol lookup",
  parameters: { type: "object", properties: { name: { type: "string" } }, required: ["name"] },
  read_only: true,
}
const mutating: CatalogToolSchema = {
  name: "tracedecay_str_replace",
  description: "Replace text in a file",
  parameters: { type: "object", properties: {} },
  read_only: false,
}

function recordingRunner(results: RunResult[] = []) {
  const calls: { args: string[]; options: RunOptions }[] = []
  const run = async (args: string[], options: RunOptions): Promise<RunResult> => {
    calls.push({ args, options })
    return results.shift() ?? { code: 0, stdout: "ok", stderr: "" }
  }
  return { calls, run }
}

function registeredTools(schemas: CatalogToolSchema[], run: ReturnType<typeof recordingRunner>["run"]) {
  const tools = new Map<string, any>()
  registerCatalogTools({ registerTool: (tool: any) => tools.set(tool.name, tool) } as any, schemas, run)
  return tools
}

function context(options: { hasUI: boolean; approve?: boolean }) {
  const prompts: string[] = []
  return {
    prompts,
    ctx: {
      cwd: "/work/project",
      hasUI: options.hasUI,
      ui: {
        confirm: async (title: string) => {
          prompts.push(title)
          return options.approve ?? false
        },
      },
    },
  }
}

test("schemas.json names are validated so none can become a CLI flag", () => {
  const entry = (name: unknown) =>
    JSON.stringify([{ ...readOnly, name }])
  expect(parseToolSchemas(entry("tracedecay_search"))[0].name).toBe("tracedecay_search")
  for (const name of ["--project=/etc", "-h", "search", "tracedecay_Search", "tracedecay_x y", 7]) {
    expect(() => parseToolSchemas(entry(name))).toThrow("invalid tool name")
  }
  expect(() => parseToolSchemas(JSON.stringify([{ name: "tracedecay_search" }]))).toThrow("malformed")
  expect(() => parseToolSchemas("{}")).toThrow("not an array")
})

test("catalog tools receive their arguments unchanged, including `name`", async () => {
  const { calls, run } = recordingRunner()
  const tools = registeredTools([readOnly, mutating], run)
  expect([...tools.keys()]).toEqual([readOnly.name, mutating.name])

  const result = await tools
    .get(readOnly.name)
    .execute("call-1", { name: "ExactName", limit: 3 }, undefined, undefined, context({ hasUI: false }).ctx)

  expect(result.content[0].text).toBe("ok")
  expect(calls).toHaveLength(1)
  expect(calls[0].args).toEqual(["tool", readOnly.name, "--args", "-"])
  expect(calls[0].options.cwd).toBe("/work/project")
  expect(JSON.parse(calls[0].options.input!)).toEqual({ name: "ExactName", limit: 3 })
})

test("read-only tools may target another registered project", async () => {
  const { calls, run } = recordingRunner()
  const tools = registeredTools([readOnly], run)
  await tools
    .get(readOnly.name)
    .execute("call", { name: "X", project_selector: { path: "/other" } }, undefined, undefined, context({ hasUI: false }).ctx)
  expect(calls.map((call) => [call.args, JSON.parse(call.options.input!)])).toEqual([
    [["tool", readOnly.name, "--args", "-"], { name: "X", project_selector: { path: "/other" } }],
  ])
})

test("mutating tools need approval and never target another project", async () => {
  const { calls, run } = recordingRunner()
  const tools = registeredTools([mutating], run)
  const execute = (args: object, ctx: object) =>
    tools.get(mutating.name).execute("call", args, undefined, undefined, ctx)

  await expect(execute({}, context({ hasUI: false }).ctx)).rejects.toThrow("needs interactive approval")

  for (const selector of [
    { project_id: "p-2" },
    { project_path: "/other" },
    { project_selector: { path: "/other" } },
    { project_selector: { project_id: "p-2" } },
  ]) {
    const approving = context({ hasUI: true, approve: true })
    await expect(execute(selector, approving.ctx)).rejects.toThrow("cross-project mutating selector")
    expect(approving.prompts).toEqual([])
  }

  const declining = context({ hasUI: true, approve: false })
  await expect(execute({ file: "a.rs" }, declining.ctx)).rejects.toThrow("was not approved")
  expect(declining.prompts).toEqual([`Allow ${mutating.name}?`])
  expect(calls).toHaveLength(0)

  await execute({ file: "a.rs" }, context({ hasUI: true, approve: true }).ctx)
  expect(calls.map((call) => call.args[1])).toEqual([mutating.name])
})

test("a stopped daemon is reported by its typed exit status, never started", async () => {
  const down = recordingRunner([
    { code: DAEMON_UNREACHABLE_EXIT_CODE, stdout: "", stderr: "Run `tracedecay daemon status`." },
  ])
  await expect(runCatalogTool(readOnly.name, {}, "/work", undefined, down.run)).rejects.toThrow(
    `${readOnly.name} unavailable: no TraceDecay daemon is serving. Run \`tracedecay daemon status\`.`,
  )
  expect(down.calls.map((call) => call.args[0])).toEqual(["tool"])

  // Daemon-sounding text on an ordinary failure is just that failure.
  const failing = recordingRunner([
    { code: 1, stdout: "", stderr: "the daemon socket is unavailable" },
  ])
  const failure = runCatalogTool(readOnly.name, {}, "/work", undefined, failing.run)
  await expect(failure).rejects.toThrow(`${readOnly.name} failed: the daemon socket is unavailable`)
  expect(failing.calls).toHaveLength(1)
})

test("child processes always see EOF on stdin", async () => {
  const startedAt = performance.now()
  const closed = await runCli([], { cwd: "/", timeoutMs: 5_000, executable: "/bin/cat" })
  expect(closed).toEqual({ code: 0, stdout: "", stderr: "" })

  const echoed = await runCli([], { cwd: "/", timeoutMs: 5_000, executable: "/bin/cat", input: "payload" })
  expect(echoed.stdout).toBe("payload")
  expect(performance.now() - startedAt).toBeLessThan(2_000)
})

test("lifecycle events reach hook-pi-event as one payload followed by EOF", async () => {
  const payload = lifecyclePayload("session_start", "session-1", "/", "startup")
  expect(payload).toMatchObject({
    hook_event_name: "session_start",
    session_id: "session-1",
    cwd: "/",
    reason: "startup",
  })
  expect(lifecyclePayload("agent_end", "session-1", "/").id).not.toBe(payload.id)
  expect("reason" in lifecyclePayload("agent_end", "session-1", "/")).toBe(false)

  const { calls, run } = recordingRunner([{ code: 0, stdout: "", stderr: "" }])
  expect(await dispatchLifecycle(payload, run)).toBeUndefined()
  expect(calls[0].args).toEqual(["hook-pi-event"])
  expect(JSON.parse(calls[0].options.input!)).toEqual(payload)

  // `cat` exits only at EOF and echoes its stdin as the hook's guidance.
  const startedAt = performance.now()
  const echoed = await dispatchLifecycle(payload, (args, options) =>
    runCli([], { ...options, executable: "/bin/cat" }),
  )
  expect(JSON.parse(echoed!)).toEqual(payload)
  expect(performance.now() - startedAt).toBeLessThan(2_000)

  const refused = recordingRunner([{ code: 1, stdout: "guidance", stderr: "refused" }])
  expect(await dispatchLifecycle(payload, refused.run)).toBeUndefined()
})
