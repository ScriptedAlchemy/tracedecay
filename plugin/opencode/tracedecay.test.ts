import { expect, test } from "bun:test"

import { dispatch, dispatchAfterAck } from "./tracedecay"

test("OpenCode dispatch returns within the hook budget when its child stalls", async () => {
  const startedAt = performance.now()

  const guidance = await dispatch("0.2", { event: "file.edited" }, "/bin/sleep")

  expect(guidance).toBeUndefined()
  expect(performance.now() - startedAt).toBeLessThan(100)
})

test("OpenCode schedules asynchronous notifications without holding the hook", () => {
  const startedAt = performance.now()

  dispatchAfterAck("0.2", { event: "session.idle" }, "/bin/sleep")

  expect(performance.now() - startedAt).toBeLessThan(25)
})

test("OpenCode dispatch accepts bounded daemon guidance", async () => {
  const guidance = await dispatch(
    "TraceDecay guidance",
    { event: "file.edited" },
    "/usr/bin/printf",
  )

  expect(guidance).toBe("TraceDecay guidance")
})

test("OpenCode dispatch drops output beyond its guidance bound", async () => {
  const guidance = await dispatch("ignored", { event: "file.edited" }, "/usr/bin/yes")

  expect(guidance).toBeUndefined()
})
