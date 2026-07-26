/**
 * Hermes mount adapter for the canonical embedded TraceDecay dashboard.
 *
 * The plugin has no dashboard UI of its own. Its backend starts the same
 * `tracedecay dashboard` server used outside Hermes, and this component mounts
 * that server in an iframe. The dashboard HTML, JavaScript, CSS, workspaces,
 * routing, and API calls therefore all come from the one `dashboard/app-dist`
 * build embedded in the TraceDecay binary.
 */
(function () {
  "use strict";

  const SDK = window.__HERMES_PLUGIN_SDK__;
  const registry = window.__HERMES_PLUGINS__;
  if (!SDK || !registry || typeof registry.register !== "function") return;

  const React = SDK.React;
  const h = React.createElement;
  const DASHBOARD_URL_ENDPOINT = "/api/plugins/tracedecay/dashboard-url";

  function TraceDecayDashboard() {
    const [url, setUrl] = React.useState(null);
    const [error, setError] = React.useState(null);

    React.useEffect(function () {
      let cancelled = false;
      SDK.fetchJSON(DASHBOARD_URL_ENDPOINT)
        .then(function (payload) {
          if (!payload || typeof payload.url !== "string" || payload.url.length === 0) {
            throw new Error("dashboard URL response was invalid");
          }
          if (!cancelled) setUrl(payload.url);
        })
        .catch(function (cause) {
          if (!cancelled) setError(String(cause));
        });
      return function () {
        cancelled = true;
      };
    }, []);

    if (error) {
      return h(
        "div",
        {
          role: "alert",
          style: {
            boxSizing: "border-box",
            minHeight: "240px",
            padding: "24px",
            color: "var(--color-error, #b42318)",
          },
        },
        "TraceDecay dashboard failed to start: " + error,
      );
    }

    if (!url) {
      return h(
        "div",
        {
          role: "status",
          style: {
            boxSizing: "border-box",
            minHeight: "240px",
            padding: "24px",
          },
        },
        "Starting TraceDecay dashboard…",
      );
    }

    return h(
      "iframe",
      {
        src: url,
        title: "TraceDecay dashboard",
        allow: "clipboard-read; clipboard-write",
        style: {
          display: "block",
          width: "100%",
          height: "calc(100vh - 56px)",
          minHeight: "640px",
          border: "0",
          background: "transparent",
        },
      },
    );
  }

  registry.register("tracedecay", TraceDecayDashboard);
})();
