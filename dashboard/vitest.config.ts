import { defineConfig } from "vitest/config";

// Foundation-lane vitest config. Scoped to the two foundation modules
// (contracts codegen + SSE reducer). The design owner replaces/extends this
// when the app shell lands; keep it minimal and framework-free.
export default defineConfig({
  test: {
    include: [
      "codegen/**/*.test.ts",
      "src/**/*.test.ts",
    ],
    environment: "node",
  },
});
