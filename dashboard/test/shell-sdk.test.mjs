import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const sdkPath = path.resolve(process.cwd(), "shell/src/sdk.jsx");
const sdk = await importBundledModule(sdkPath);

function withMockedFetch(mock, run) {
  const hadFetch = Object.prototype.hasOwnProperty.call(globalThis, "fetch");
  const originalFetch = globalThis.fetch;
  globalThis.fetch = mock;
  return Promise.resolve()
    .then(run)
    .finally(() => {
      if (hadFetch) {
        globalThis.fetch = originalFetch;
      } else {
        delete globalThis.fetch;
      }
    });
}

test("fetchJSON returns parsed body on success", async () => {
  const seen = [];
  await withMockedFetch(
    async (url, init) => {
      seen.push([url, init]);
      return {
        ok: true,
        json: async () => ({ ok: true, value: 7 }),
      };
    },
    async () => {
      const body = await sdk.fetchJSON("/ok", { method: "POST" });
      assert.deepEqual(body, { ok: true, value: 7 });
    },
  );
  assert.deepEqual(seen, [["/ok", { method: "POST" }]]);
});

test("fetchJSON prefers JSON detail on failure", async () => {
  const body = {
    detail: "token expired",
    validation_errors: [{ field: "token", message: "token expired" }],
  };
  await withMockedFetch(
    async () => ({
      ok: false,
      status: 403,
      statusText: "Forbidden",
      json: async () => body,
    }),
    async () => {
      await assert.rejects(
        async () => sdk.fetchJSON("/nope"),
        (err) => {
          assert.match(err.message, /token expired/);
          assert.deepEqual(err.body, body);
          return true;
        },
      );
    },
  );
});

test("fetchJSON falls back to status text when body is non-JSON", async () => {
  await withMockedFetch(
    async () => ({
      ok: false,
      status: 500,
      statusText: "Server Error",
      json: async () => {
        throw new Error("not json");
      },
    }),
    async () => {
      await assert.rejects(() => sdk.fetchJSON("/boom"), /500 Server Error/);
    },
  );
});

test("Button resolves variant precedence and size class", () => {
  const button = sdk.Button({
    ghost: true,
    outlined: true,
    secondary: true,
    size: "sm",
    className: "custom",
    children: "Click",
  });
  assert.equal(button.type, "button");
  assert.match(button.props.className, /\bts-button\b/);
  assert.match(button.props.className, /\bts-button-ghost\b/);
  assert.match(button.props.className, /\bts-button-sm\b/);
  assert.doesNotMatch(button.props.className, /\bts-button-outline\b/);
  assert.doesNotMatch(button.props.className, /\bts-button-secondary\b/);
});

test("Button explicit variant overrides flag-based variant", () => {
  const button = sdk.Button({
    variant: "outline",
    destructive: true,
    ghost: true,
  });
  assert.match(button.props.className, /\bts-button-outline\b/);
  assert.doesNotMatch(button.props.className, /\bts-button-destructive\b/);
  assert.doesNotMatch(button.props.className, /\bts-button-ghost\b/);
});

test("cn flattens nested values and keeps non-empty strings", () => {
  const className = sdk.cn("a", ["b", null, ["c", 0, ""]], false, "d");
  assert.equal(className, "a b c d");
});

test("timeAgo and isoTimeAgo format recent and stale timestamps", () => {
  const realNow = Date.now;
  Date.now = () => 1_700_000_000_000;
  try {
    assert.equal(sdk.timeAgo(1_700_000_000), "just now");
    assert.equal(sdk.timeAgo(1_699_999_640), "6m ago");
    assert.equal(sdk.timeAgo(1_699_991_000), "2h ago");
    // ~1.5 days ago: both formatters share the same ladder, including the
    // "yesterday" bucket.
    assert.equal(sdk.timeAgo(1_700_000_000 - 130_000), "yesterday");
    assert.equal(sdk.timeAgo(1_700_000_000 - 200_000), "2d ago");
    assert.equal(sdk.isoTimeAgo("2023-11-14T22:13:20.000Z"), "just now");
    assert.equal(sdk.isoTimeAgo("2023-11-13T10:00:00.000Z"), "yesterday");
    assert.equal(sdk.isoTimeAgo("2099-01-01T00:00:00.000Z"), "unknown");
    assert.equal(sdk.isoTimeAgo("not a date"), "unknown");
  } finally {
    Date.now = realNow;
  }
});

test("buildSDK exposes makeSequence on utils", () => {
  const built = sdk.buildSDK();
  assert.equal(typeof built.utils.makeSequence, "function");
  assert.equal(built.utils.makeSequence, sdk.makeSequence);
});
