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

// React's automatic *dev* runtime (`react/jsx-dev-runtime`) is aliased to this
// module, so it must also export `jsxDEV`. We don't need the dev-only
// diagnostics (source/self), so delegate to the prod factories; static children
// route through `jsxs` so multiple children spread exactly as in production.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function jsxDEV(type: any, props: any, key?: any, isStaticChildren?: boolean) {
  return isStaticChildren ? jsxs(type, props, key) : jsx(type, props, key);
}
