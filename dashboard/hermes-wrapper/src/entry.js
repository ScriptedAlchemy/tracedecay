/**
 * TraceDecay dashboard for Hermes — thin wrapper over the tracedecay dashboards.
 *
 * This is the Hermes-hosted variant of the canonical TraceDecay dashboard
 * (see the tracedecay repo, `dashboard/`). It does NOT fork the UI bundles:
 * the holographic, LCM, code-graph, and savings bundles shipped in this
 * plugin's dist/ are byte-identical to the ones `tracedecay dashboard`
 * serves. The wrapper:
 *
 *   1. evaluates each child bundle against a window Proxy whose
 *      `__HERMES_PLUGIN_SDK__.fetchJSON` rewrites the child's API base
 *      (`/api/plugins/holographic`, `/api/plugins/hermes-lcm`,
 *      `/api/plugins/graph`, `/api/plugins/savings`) onto this plugin's API
 *      prefix (`/api/plugins/tracedecay/...`), which plugin_api.py
 *      reverse-proxies to a local `tracedecay dashboard` server;
 *   2. captures the components the child bundles register (without touching
 *      the real host registry, so other Hermes plugins are unaffected);
 *   3. registers one combined, tabbed page as "tracedecay".
 *
 * Future Hermes-specific capabilities should be added as extra endpoints in
 * plugin_api.py and advertised through /capabilities (see the tracedecay
 * dashboard handoff doc), not by forking the UI bundles.
 */
(function () {
  "use strict";

  const realWindow = window;
  const SDK = realWindow.__HERMES_PLUGIN_SDK__;
  const registry = realWindow.__HERMES_PLUGINS__;
  if (!SDK || !registry || typeof registry.register !== "function") return;

  const PLUGIN = "tracedecay";
  const ASSET_BASE = "/dashboard-plugins/" + PLUGIN + "/dist";
  const API_REWRITES = [
    ["/api/plugins/holographic", "/api/plugins/" + PLUGIN + "/holographic"],
    ["/api/plugins/hermes-lcm", "/api/plugins/" + PLUGIN + "/lcm"],
    ["/api/plugins/graph", "/api/plugins/" + PLUGIN + "/graph"],
    ["/api/plugins/savings", "/api/plugins/" + PLUGIN + "/savings"],
  ];

  function rewriteUrl(url) {
    if (typeof url !== "string") return url;
    for (const [from, to] of API_REWRITES) {
      if (
        url === from ||
        url.startsWith(from + "/") ||
        url.startsWith(from + "?")
      ) {
        return to + url.slice(from.length);
      }
    }
    return url;
  }

  const patchedSDK = Object.assign({}, SDK, {
    fetchJSON: (url, init) => SDK.fetchJSON(rewriteUrl(url), init),
    authedFetch: (url, init) => SDK.authedFetch(rewriteUrl(url), init),
  });

  /** Components registered by the child bundles, keyed by their plugin name. */
  const captured = new Map();
  let listeners = [];
  const sandboxGlobals = {
    __HERMES_PLUGIN_SDK__: patchedSDK,
    __HERMES_PLUGINS__: {
      register: function (name, component) {
        captured.set(name, component);
        listeners.forEach(function (fn) {
          try {
            fn();
          } catch (err) {
            /* ignore */
          }
        });
      },
      registerSlot: function () {},
    },
  };

  // Window facade for child bundles: overrides only the SDK/registry globals;
  // everything else forwards to the real window (functions re-bound so DOM
  // APIs keep their receiver).
  const windowProxy = new Proxy(realWindow, {
    get(target, prop) {
      if (Object.prototype.hasOwnProperty.call(sandboxGlobals, prop)) {
        return sandboxGlobals[prop];
      }
      const value = Reflect.get(target, prop);
      return typeof value === "function" ? value.bind(target) : value;
    },
    set(target, prop, value) {
      if (Object.prototype.hasOwnProperty.call(sandboxGlobals, prop)) {
        sandboxGlobals[prop] = value;
        return true;
      }
      return Reflect.set(target, prop, value);
    },
    has(target, prop) {
      return (
        Object.prototype.hasOwnProperty.call(sandboxGlobals, prop) ||
        prop in target
      );
    },
  });

  let loadPromise = null;
  function loadChildren() {
    if (!loadPromise) {
      loadPromise = Promise.all(
        ["holographic.js", "lcm.js", "graph.js", "savings.js"].map(function (file) {
          return fetch(ASSET_BASE + "/" + file, { cache: "no-store" })
            .then(function (res) {
              if (!res.ok) throw new Error(file + ": HTTP " + res.status);
              return res.text();
            })
            .then(function (code) {
              // Evaluated against the proxy so the bundle sees the patched
              // SDK without the real window globals ever being mutated.
              new Function("window", "self", "globalThis", code)(
                windowProxy,
                windowProxy,
                windowProxy,
              );
            });
        }),
      );
    }
    return loadPromise;
  }

  const React = SDK.React;
  const h = React.createElement;
  const TABS = [
    ["holographic", "Memory"],
    ["hermes-lcm", "LCM"],
    ["graph", "Code Graph"],
    ["savings", "Savings"],
  ];

  function TraceDecayPage() {
    const [active, setActive] = React.useState("holographic");
    const [error, setError] = React.useState("");
    const [tick, setTick] = React.useState(0);

    React.useEffect(function () {
      let cancelled = false;
      const bump = function () {
        if (!cancelled) setTick(function (t) { return t + 1; });
      };
      listeners.push(bump);
      loadChildren().catch(function (err) {
        if (!cancelled) setError(String(err));
      });
      return function () {
        cancelled = true;
        listeners = listeners.filter(function (fn) { return fn !== bump; });
      };
    }, []);

    const Active = captured.get(active);
    return h(
      "div",
      { className: "tsiw-root", "data-tick": tick },
      h(
        "div",
        { className: "tsiw-tabs" },
        TABS.map(function (tab) {
          return h(
            "button",
            {
              key: tab[0],
              className:
                "tsiw-tab" + (tab[0] === active ? " tsiw-tab-active" : ""),
              onClick: function () {
                setActive(tab[0]);
              },
            },
            tab[1],
          );
        }),
      ),
      error
        ? h("div", { className: "tsiw-error" }, "Failed to load: " + error)
        : Active
          ? h(Active)
          : h("div", { className: "tsiw-loading" }, "Loading tracedecay dashboards…"),
    );
  }

  registry.register(PLUGIN, TraceDecayPage);
})();
