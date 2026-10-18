import { resolve } from 'node:path';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  // Bazel exposes declared source inputs as symlinks within the test runfiles.
  resolve: { preserveSymlinks: true },
  test: {
    projects: [
      {
        extends: true,
        test: {
          name: 'node',
          environment: 'node',
          include: ['codegen/**/*.test.ts', 'src/**/*.test.ts', 'stories/**/*.test.ts'],
          exclude: ['**/node_modules/**', '**/*.dom.test.{ts,tsx}'],
        },
      },
      {
        extends: true,
        test: {
          name: 'dom',
          environment: 'jsdom',
          include: ['src/**/*.dom.test.{ts,tsx}', 'e2e/**/*.dom.test.ts'],
          // Keep the setup entry inside Bazel runfiles before Vite resolves it.
          setupFiles: [resolve('vitest.setup.dom.ts')],
        },
      },
    ],
  },
});
