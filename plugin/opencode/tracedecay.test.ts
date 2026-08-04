import { expect, test } from "bun:test"

import { dispatch, dispatchAfterAck } from "./tracedecay"

test("hook callback schedules the child before the fast ACK deadline", () => {
  const started = performance.now()

  dispatchAfterAck("", { private_event: "never logged" }, "/bin/true")

  expect(performance.now() - started).toBeLessThan(25)
})

test("scheduled dispatch kills a child that exceeds the private budget", async () => {
  expect(await dispatch("2", { private_event: "never logged" }, "/bin/sleep")).toBeUndefined()
})

test("hook dispatch drops output that exceeds its private guidance limit", async () => {
  const result = await dispatch("", { private_event: "never logged" }, "/usr/bin/yes")

  expect(result).toBeUndefined()
})

test("hook dispatch returns bounded daemon guidance", async () => {
  const result = await dispatch("bounded guidance", { private_event: "never logged" }, "/usr/bin/printf")

  expect(result).toBe("bounded guidance")
})
