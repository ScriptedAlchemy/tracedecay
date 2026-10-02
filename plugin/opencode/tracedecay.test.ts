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
  pending.deliveryFor("ses_a")("first")
  pending.deliveryFor("ses_a")(undefined)
  pending.deliveryFor("ses_a")("second")
  pending.deliveryFor("ses_b")("other")

  expect(pending.drain("ses_a")).toEqual(["first", "second"])
  expect(pending.drain("ses_a")).toEqual([])
  expect(pending.drain("ses_b")).toEqual(["other"])
})

test("session deletion discards queued guidance and invalidates in-flight delivery", () => {
  const pending = new PendingGuidance()
  pending.deliveryFor("ses_deleted")("queued before deletion")
  const deliver = pending.deliveryFor("ses_deleted")
  pending.delete("ses_deleted")
  deliver("late callback after deletion")

  expect(pending.drain("ses_deleted")).toEqual([])
  pending.deliveryFor("ses_other")("still active")
  expect(pending.drain("ses_other")).toEqual(["still active"])
})

test("draining a turn does not invalidate guidance still in flight for a live session", () => {
  const pending = new PendingGuidance()
  const first = pending.deliveryFor("ses_active")
  const deliver = pending.deliveryFor("ses_active")
  first("first turn")
  expect(pending.drain("ses_active")).toEqual(["first turn"])
  deliver("next turn")
  expect(pending.drain("ses_active")).toEqual(["next turn"])
})

test("an old callback cannot deliver to a new session with the same identifier", () => {
  const pending = new PendingGuidance()
  const oldDelivery = pending.deliveryFor("ses_reused")
  pending.delete("ses_reused")
  pending.deliveryFor("ses_reused")("new session")
  oldDelivery("deleted session")

  expect(pending.drain("ses_reused")).toEqual(["new session"])
})

test("inactive sessions cannot grow the guidance store indefinitely", () => {
  const pending = new PendingGuidance()
  const oldestDelivery = pending.deliveryFor("ses_oldest")
  oldestDelivery("oldest")
  for (let index = 0; index < 1024; index++) {
    pending.deliveryFor(`ses_${index}`)("recent")
  }
  oldestDelivery("late callback for evicted session")

  expect(pending.drain("ses_oldest")).toEqual([])
  expect(pending.drain("ses_1023")).toEqual(["recent"])
})

test("empty sessions cannot evict undelivered guidance", () => {
  const pending = new PendingGuidance()
  pending.deliveryFor("ses_waiting")("fix this edit")
  for (let index = 0; index < 127; index++) {
    pending.deliveryFor(`ses_empty_${index}`)("delivered")
    pending.drain(`ses_empty_${index}`)
  }
  pending.deliveryFor("ses_new")("new guidance")

  expect(pending.drain("ses_waiting")).toEqual(["fix this edit"])
  expect(pending.drain("ses_new")).toEqual(["new guidance"])
})

test("capacity pressure does not invalidate an unfinished hook delivery", () => {
  const pending = new PendingGuidance()
  pending.deliveryFor("ses_oldest")("oldest guidance")
  const unfinished = pending.deliveryFor("ses_unfinished")
  for (let index = 0; index < 126; index++) {
    pending.deliveryFor(`ses_ready_${index}`)("ready")
  }
  pending.deliveryFor("ses_new")("new guidance")
  unfinished("late guidance")

  expect(pending.drain("ses_unfinished")).toEqual(["late guidance"])
  expect(pending.drain("ses_new")).toEqual(["new guidance"])
})

test("all outstanding hooks can finish even when they exceed idle retention capacity", () => {
  const pending = new PendingGuidance()
  const deliveries = Array.from({ length: 256 }, (_, index) => pending.deliveryFor(`ses_${index}`))
  for (let index = 0; index < deliveries.length; index++) {
    deliveries[index]("completed")
    expect(pending.drain(`ses_${index}`)).toEqual(["completed"])
  }
})

test("each hook delivery settles once without releasing another outstanding hook", () => {
  const pending = new PendingGuidance()
  const first = pending.deliveryFor("ses_active")
  const second = pending.deliveryFor("ses_active")
  first("first")
  first("duplicate")
  expect(pending.drain("ses_active")).toEqual(["first"])
  second("second")
  expect(pending.drain("ses_active")).toEqual(["second"])
})

test("plugin disposal clears guidance and invalidates every in-flight delivery", () => {
  const pending = new PendingGuidance()
  pending.deliveryFor("ses_active")("queued")
  const deliver = pending.deliveryFor("ses_active")
  pending.clear()
  deliver("late")

  expect(pending.drain("ses_active")).toEqual([])
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
