import { expect, test } from "vitest";

import config from "../rsbuild.config";

test("PostCSS configuration invalidates the persistent build cache", () => {
  expect(config.performance?.buildCache).toEqual({
    buildDependencies: ["postcss.config.mjs"],
  });
});
