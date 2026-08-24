import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./test/vitest.setup.mjs"],
    include: ["test/**/*.vitest.{js,mjs,ts,tsx}"],
    restoreMocks: true,
    clearMocks: true,
  },
});
