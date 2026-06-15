/**
 * Automatic-JSX runtime shim.
 *
 * esbuild is built with `jsx: "automatic"`, so it emits imports of
 * `jsx`/`jsxs`/`Fragment` from `react/jsx-runtime`. We alias that specifier to
 * this module, which implements the runtime on top of the host's
 * `React.createElement` (pulled from `./react-shim`). This keeps the plugin off
 * a bundled jsx-runtime and on the host's React instance.
 */

import React from "react";

export const Fragment = React.Fragment;

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function jsx(type: any, props: any, key?: any) {
  const { children, ...rest } = props || {};
  if (key !== undefined) rest.key = key;
  return children === undefined
    ? React.createElement(type, rest)
    : React.createElement(type, rest, children);
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function jsxs(type: any, props: any, key?: any) {
  const { children, ...rest } = props || {};
  if (key !== undefined) rest.key = key;
  const kids = Array.isArray(children) ? children : [children];
  return React.createElement(type, rest, ...kids);
}
