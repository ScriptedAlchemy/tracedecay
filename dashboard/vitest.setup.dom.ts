// DOM-project setup (jsdom). Auto-unmount React trees after every test so
// queries never leak between cases. Kept dependency-light: no jest-dom matchers
// are pulled in — DOM tests assert with Testing Library queries and plain
// vitest expectations.
import { afterEach } from 'vitest';
import { cleanup } from '@testing-library/react';

// Sigma reads `WebGL2RenderingContext` at MODULE scope, so merely importing a
// component whose module graph reaches it throws `ReferenceError` under jsdom —
// before any test body runs, and regardless of whether that test touches the
// canvas. jsdom ships no WebGL, so the constructor is declared here purely to
// let the module evaluate. It is deliberately not a working implementation:
// components probe for a real context (`hasWebGl`) and take their no-WebGL
// path, which is the behavior jsdom should exercise anyway.
for (const name of ['WebGLRenderingContext', 'WebGL2RenderingContext']) {
  if (!(name in globalThis)) {
    Object.defineProperty(globalThis, name, {
      configurable: true,
      writable: true,
      value: class {},
    });
  }
}

// jsdom implements no layout, so it ships no `ResizeObserver` either. Declared
// as an observer that never fires, which is the honest jsdom reading rather
// than a stand-in: nothing here has a size, so nothing can change size. Code
// that measures the page therefore takes its "not measured" path under jsdom
// and is proved in a browser instead.
if (!('ResizeObserver' in globalThis)) {
  Object.defineProperty(globalThis, 'ResizeObserver', {
    configurable: true,
    writable: true,
    value: class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  });
}

// jsdom implements no layout, and therefore none of the element scrolling API:
// `scrollIntoView` and `scrollTo` are absent from `Element.prototype` rather
// than present and inert. Both are always there in a browser, so guarding the
// calls in product code would add branches no browser can take — and ones that
// would silently stop scrolling if a browser ever did. Declared here as the
// no-ops jsdom would have had, which also gives tests something to spy on when
// the call itself is the observable.
//
// Their absence is why nothing could cover the two surfaces that scroll: the
// palette keeping its keyboard selection visible, and the Settings section
// index jumping to a group. A DOM test of either threw here rather than
// exercising the behavior.
for (const name of ['scrollIntoView', 'scrollTo'] as const) {
  if (typeof Element.prototype[name] !== 'function') {
    Element.prototype[name] = function scrollNoop() {};
  }
}

afterEach(() => {
  cleanup();
});
