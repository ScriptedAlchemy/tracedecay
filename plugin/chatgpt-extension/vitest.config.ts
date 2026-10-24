import { defineConfig } from "vitest/config";

export default defineConfig({
  // Keep DOM test imports inside Bazel's declared runfiles tree.
  resolve: { preserveSymlinks: true },
  test: {
    include: ["test/**/*.test.ts"],
    testTimeout: 180_000,
    hookTimeout: 180_000,
    fileParallelism: false,
  },
});
