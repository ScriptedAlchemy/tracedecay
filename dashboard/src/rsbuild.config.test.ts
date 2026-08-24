import { expect, test } from "vitest";

import config from "../rsbuild.config";

test("production builds do not use a persistent cache that drifts content hashes", () => {
  expect(config.performance?.buildCache).toBe(false);
});
