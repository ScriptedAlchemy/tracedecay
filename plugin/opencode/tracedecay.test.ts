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

test("dispatch returns guidance only after the hook child exits successfully", async () => {
  const guidance = await dispatch(
    "TraceDecay guidance",
    { type: "session.execution.succeeded" },
    "/usr/bin/printf",
  )
  expect(guidance).toBe("TraceDecay guidance")
})

test("dispatch runs the hook child in the plugin location so the daemon resolves the project", async () => {
  const guidance = await dispatch("", {}, "/bin/pwd", "/tmp")

  expect(guidance).toBe("/tmp")
})

test("OpenCode acknowledges its callback before child guidance is delivered", async () => {
  let callbackReturned = false
  const delivered = new Promise<{ guidance: string | undefined; callbackReturned: boolean }>(
    (resolve) => {
      dispatchAfterAck(
        "TraceDecay guidance",
        { type: "session.execution.succeeded" },
        (guidance) => resolve({ guidance, callbackReturned }),
        "/usr/bin/printf",
      )
    },
  )

  callbackReturned = true
  expect(await delivered).toEqual({ guidance: "TraceDecay guidance", callbackReturned: true })
})
