// DOM-project setup (jsdom). Auto-unmount React trees after every test so
// queries never leak between cases. Kept dependency-light: no jest-dom matchers
// are pulled in — DOM tests assert with Testing Library queries and plain
// vitest expectations.
import { afterEach } from 'vitest';
import { cleanup } from '@testing-library/react';

afterEach(() => {
  cleanup();
});
