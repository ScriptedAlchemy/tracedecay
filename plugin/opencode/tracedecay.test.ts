import { expect, test } from "bun:test"

import mcpModule from "./tracedecay-mcp"
import hookModule, {
  PendingGuidance,
  SessionLocations,
  TraceDecayPlugin,
  dispatch,
  dispatchAfterAck,
} from "./tracedecay"

const HERE = "/repo/here"
const ELSEWHERE = "/repo/elsewhere"

function located(type: string, sessionID: string, directory: string) {
  return { type, location: { directory }, data: { sessionID } }
}

function boundary(sessionID: string, type = "session.execution.succeeded") {
  return { type, data: { sessionID } }
}

test("OpenCode discovers one V2 definition per installed module", () => {
  expect(hookModule).toBe(TraceDecayPlugin)
  expect(hookModule.id).toBe("tracedecay-hooks")
  expect(typeof hookModule.setup).toBe("function")
  expect(mcpModule.id).toBe("tracedecay-mcp")
  expect(typeof mcpModule.setup).toBe("function")
})

test("only the owning location dispatches an unlocated execution boundary", () => {
  const sessions = new SessionLocations(HERE)

  expect(sessions.isSessionBoundary(boundary("ses_unseen"))).toBeFalse()

  sessions.observe(located("session.created", "ses_here", HERE))
  sessions.observe(located("session.tool.success", "ses_there", ELSEWHERE))
  expect(sessions.isSessionBoundary(boundary("ses_here"))).toBeTrue()
  expect(sessions.isSessionBoundary(boundary("ses_here", "session.execution.failed"))).toBeTrue()
  expect(sessions.isSessionBoundary(boundary("ses_here", "session.execution.interrupted"))).toBeTrue()
  expect(sessions.isSessionBoundary(boundary("ses_there"))).toBeFalse()
  expect(sessions.isSessionBoundary(boundary("ses_here", "session.execution.started"))).toBeFalse()

  sessions.observe({ type: "session.deleted", data: { sessionID: "ses_here" } })
  expect(sessions.isSessionBoundary(boundary("ses_here"))).toBeFalse()
})

test("guidance waits for its own session's next model request and is delivered once", () => {
  const pending = new PendingGuidance()
  pending.push("ses_a", "first")
  pending.push("ses_a", undefined)
  pending.push("ses_a", "second")
  pending.push("ses_b", "other")

  expect(pending.drain("ses_a")).toEqual(["first", "second"])
  expect(pending.drain("ses_a")).toEqual([])
  expect(pending.drain("ses_b")).toEqual(["other"])
})

test("dispatch lets the hook child finish instead of killing before durable spool", async () => {
  const guided = await dispatch(
    "TraceDecay guidance",
    { type: "session.execution.succeeded" },
    "/usr/bin/printf",
  )
  expect(guided).toBe("TraceDecay guidance")

  const startedAt = performance.now()
  const guidance = await dispatch("0.05", { type: "session.execution.succeeded" }, "/bin/sleep")

  expect(guidance).toBeUndefined()
  expect(performance.now() - startedAt).toBeGreaterThanOrEqual(40)
})

test("dispatch runs the hook child in the plugin location so the daemon resolves the project", async () => {
  const guidance = await dispatch("", {}, "/bin/pwd", "/tmp")

  expect(guidance).toBe("/tmp")
})

test("OpenCode acknowledges its callback while the durable child continues", async () => {
  const startedAt = performance.now()
  let delivered = false

  dispatchAfterAck(
    "0.05",
    { type: "session.execution.succeeded" },
    () => {
      delivered = true
    },
    "/bin/sleep",
  )

  expect(performance.now() - startedAt).toBeLessThan(25)
  await Bun.sleep(100)
  expect(delivered).toBeTrue()
})
