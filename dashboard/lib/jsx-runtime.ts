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
