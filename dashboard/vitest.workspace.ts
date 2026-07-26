import { defineWorkspace } from 'vitest/config';

// Two vitest projects (plan 11a R56): the existing framework-free unit tests
// keep running under the fast `node` environment, and DOM tests (Testing
// Library) run under `jsdom`. DOM tests use the `*.dom.test.{ts,tsx}` suffix so
// the two projects never overlap — the node project excludes them explicitly
// and the jsdom project includes only them. Each project is self-contained
// (no `extends`) because vitest concatenates inherited `include` globs, which
// would otherwise pull every node test into the jsdom project. JSX transform is
// inferred from tsconfig.json (`jsx: react-jsx`) by Vite in both projects.
export default defineWorkspace([
  {
    test: {
      name: 'node',
      environment: 'node',
      include: ['codegen/**/*.test.ts', 'src/**/*.test.ts'],
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
]);
