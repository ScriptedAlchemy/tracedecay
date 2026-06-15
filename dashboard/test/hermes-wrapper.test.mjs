import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import vm from "node:vm";
import { readFile } from "node:fs/promises";

const wrapperPath = path.resolve(process.cwd(), "hermes-wrapper/src/entry.js");
const assetBase = "/dashboard-plugins/tracedecay/dist";

function createReactStub() {
  return {
    createElement(type, props, ...children) {
      return {
        type,
        props: { ...(props || {}), children: children.length <= 1 ? children[0] : children },
      };
    },
    useState(initial) {
      let value = typeof initial === "function" ? initial() : initial;
      return [
        value,
        (next) => {
          value = typeof next === "function" ? next(value) : next;
        },
      ];
    },
    useEffect(run) {
      run();
    },
  };
}

async function loadWrapper({ scriptsByUrl }) {
  const source = await readFile(wrapperPath, "utf8");
  const registryCalls = [];
  const fetchJsonCalls = [];
  const authedFetchCalls = [];

  const realWindow = {
    boundProbe() {
      return this === realWindow ? "bound" : "unbound";
    },
  };

  realWindow.__HERMES_PLUGIN_SDK__ = {
    React: createReactStub(),
    fetchJSON: (url, init) => {
      fetchJsonCalls.push([url, init]);
      return Promise.resolve({ ok: true });
    },
    authedFetch: (url, init) => {
      authedFetchCalls.push([url, init]);
      return Promise.resolve({ ok: true });
    },
  };

  const registeredPlugins = new Map();
  realWindow.__HERMES_PLUGINS__ = {
    register: (name, component) => {
      registryCalls.push(name);
      registeredPlugins.set(name, component);
    },
    registerSlot: () => {},
  };

  const fetchCalls = [];
  const context = {
    window: realWindow,
    fetch: async (url) => {
      const key = String(url);
      fetchCalls.push(key);
      if (!(key in scriptsByUrl)) {
        return {
          ok: false,
          status: 404,
          text: async () => "",
        };
      }
      return {
        ok: true,
        status: 200,
        text: async () => scriptsByUrl[key],
      };
    },
    Promise,
    setTimeout,
    clearTimeout,
  };

  vm.runInNewContext(source, context, { filename: "hermes-wrapper/src/entry.js" });
  return {
    realWindow,
    registeredPlugins,
    registryCalls,
    fetchCalls,
    fetchJsonCalls,
    authedFetchCalls,
  };
}

async function flushAsync() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test("wrapper rewrites child API calls and keeps child registrations isolated", async () => {
  const scripts = {
    [`${assetBase}/holographic.js`]: `
window.__HERMES_PLUGIN_SDK__.fetchJSON("/api/plugins/holographic");
window.__HERMES_PLUGIN_SDK__.fetchJSON("/api/plugins/holographic/similarity?limit=2");
window.__HERMES_PLUGIN_SDK__.fetchJSON("/api/plugins/other");
window.__HERMES_PLUGIN_SDK__.authedFetch("/api/plugins/hermes-lcm/search?q=abc");
window.__boundResult = window.boundProbe();
window.__HERMES_PLUGINS__.register("holographic", function Holographic(){ return null; });
`,
    [`${assetBase}/lcm.js`]: `
window.__HERMES_PLUGIN_SDK__.fetchJSON("/api/plugins/hermes-lcm/overview");
window.__HERMES_PLUGINS__.register("hermes-lcm", function Lcm(){ return null; });
`,
    [`${assetBase}/graph.js`]: `
window.__HERMES_PLUGIN_SDK__.fetchJSON("/api/plugins/graph");
window.__HERMES_PLUGIN_SDK__.fetchJSON("/api/plugins/graph/nodes?limit=5");
window.__HERMES_PLUGIN_SDK__.authedFetch("/api/plugins/graph/search?q=fn");
window.__HERMES_PLUGINS__.register("graph", function Graph(){ return null; });
`,
    [`${assetBase}/savings.js`]: `
window.__HERMES_PLUGIN_SDK__.fetchJSON("/api/plugins/savings/overview");
window.__HERMES_PLUGIN_SDK__.fetchJSON("/api/plugins/savings/ledger?range=30d");
window.__HERMES_PLUGINS__.register("savings", function Savings(){ return null; });
`,
  };

  const loaded = await loadWrapper({ scriptsByUrl: scripts });
  const page = loaded.registeredPlugins.get("tracedecay");
  assert.equal(typeof page, "function");

  page();
  await flushAsync();

  assert.deepEqual(loaded.registryCalls, ["tracedecay"]);
  assert.equal(loaded.realWindow.__boundResult, "bound");
  assert.deepEqual(
    loaded.fetchCalls.sort(),
    [
      `${assetBase}/holographic.js`,
      `${assetBase}/lcm.js`,
      `${assetBase}/graph.js`,
      `${assetBase}/savings.js`,
    ].sort(),
  );

  assert.deepEqual(
    loaded.fetchJsonCalls.map(([url]) => url).sort(),
    [
      "/api/plugins/tracedecay/holographic",
      "/api/plugins/tracedecay/holographic/similarity?limit=2",
      "/api/plugins/other",
      "/api/plugins/tracedecay/lcm/overview",
      "/api/plugins/tracedecay/graph",
      "/api/plugins/tracedecay/graph/nodes?limit=5",
      "/api/plugins/tracedecay/savings/overview",
      "/api/plugins/tracedecay/savings/ledger?range=30d",
    ].sort(),
  );
  assert.deepEqual(loaded.authedFetchCalls.map(([url]) => url).sort(), [
    "/api/plugins/tracedecay/graph/search?q=fn",
    "/api/plugins/tracedecay/lcm/search?q=abc",
  ].sort());
});

test("wrapper exits early when host globals are missing", async () => {
  const source = await readFile(wrapperPath, "utf8");
  const context = {
    window: {},
    fetch: async () => ({
      ok: true,
      status: 200,
      text: async () => "",
    }),
  };
  assert.doesNotThrow(() => {
    vm.runInNewContext(source, context, { filename: "hermes-wrapper/src/entry.js" });
  });
});
