import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: 'node',
          environment: 'node',
          include: ['codegen/**/*.test.ts', 'src/**/*.test.ts', 'stories/**/*.test.ts'],
          exclude: ['**/node_modules/**', '**/*.dom.test.{ts,tsx}'],
        },
      },
      {
        test: {
          name: 'dom',
          environment: 'jsdom',
          include: ['src/**/*.dom.test.{ts,tsx}', 'e2e/**/*.dom.test.ts'],
          setupFiles: ['./vitest.setup.dom.ts'],
        },
      },
    ],
  },
});
