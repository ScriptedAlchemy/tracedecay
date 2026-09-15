import { expect, test } from "bun:test"

import mcpModule from "./tracedecay-mcp"
import hookModule, { dispatch, dispatchAfterAck, TraceDecayPlugin } from "./tracedecay"

test("OpenCode discovers one v1 server entrypoint per installed module", () => {
  expect(hookModule).toEqual({ id: "tracedecay-hooks", server: TraceDecayPlugin })
  expect(mcpModule.id).toBe("tracedecay-mcp")
  expect(typeof mcpModule.server).toBe("function")
})

test("dispatch lets the hook child finish instead of killing before durable spool", async () => {
  const startedAt = performance.now()

  const guidance = await dispatch("0.05", { event: "file.edited" }, "/bin/sleep")

  expect(guidance).toBeUndefined()
  expect(performance.now() - startedAt).toBeGreaterThanOrEqual(40)
})

test("OpenCode acknowledges its callback while the durable child continues", async () => {
  const startedAt = performance.now()
  let delivered = false

  dispatchAfterAck(
    "0.05",
    { event: "session.idle" },
    async () => {
      delivered = true
    },
    "/bin/sleep",
  )

  expect(performance.now() - startedAt).toBeLessThan(25)
  await Bun.sleep(100)
  expect(delivered).toBeTrue()
})

test("dispatch accepts bounded daemon guidance", async () => {
  const guidance = await dispatch(
    "TraceDecay guidance",
    { event: "file.edited" },
    "/usr/bin/printf",
  )

  expect(guidance).toBe("TraceDecay guidance")
})
