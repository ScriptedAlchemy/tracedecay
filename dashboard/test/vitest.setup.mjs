import React from "react";

if (!globalThis.PointerEvent) {
  globalThis.PointerEvent = MouseEvent;
}

if (!globalThis.ResizeObserver) {
  globalThis.ResizeObserver = class ResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

if (!globalThis.requestAnimationFrame) {
  globalThis.requestAnimationFrame = (callback) => setTimeout(() => callback(Date.now()), 0);
  globalThis.cancelAnimationFrame = (id) => clearTimeout(id);
}

if (!HTMLElement.prototype.scrollIntoView) {
  HTMLElement.prototype.scrollIntoView = () => {};
}

const passthrough = (tagName) => {
  const Component = React.forwardRef(({ children, ...props }, ref) =>
    React.createElement(tagName, { ref, ...props }, children),
  );
  Component.displayName = `SDK${tagName}`;
  return Component;
};

const Button = React.forwardRef(({ children, ghost, outlined, variant, className, ...props }, ref) =>
  React.createElement(
    "button",
    {
      ref,
      "data-ghost": ghost ? "true" : undefined,
      "data-outlined": outlined ? "true" : undefined,
      "data-variant": variant,
      className,
      ...props,
    },
    children,
  ),
);
Button.displayName = "SDKButton";

const Input = React.forwardRef((props, ref) => React.createElement("input", { ref, ...props }));
Input.displayName = "SDKInput";

window.__HERMES_PLUGIN_SDK__ = {
  fetchJSON: async () => {
    throw new Error("fetchJSON not mocked in test");
  },
  components: {
    Card: passthrough("div"),
    CardHeader: passthrough("div"),
    CardTitle: passthrough("div"),
    CardContent: passthrough("div"),
    Badge: passthrough("span"),
    Button,
    Input,
  },
  utils: {
    cn: (...values) => values.filter(Boolean).join(" "),
    timeAgo: (ts) => new Date(ts * 1000).toLocaleString(),
  },
};
