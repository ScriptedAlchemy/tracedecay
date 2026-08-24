import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT,
  MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT,
  MAX_NATIVE_DIAGNOSTIC_MESSAGE_BYTES,
  admitNativeWorkspace,
  createNativeDiagnosticsPayload,
  batchAdmittedNativeDiagnosticDocuments,
  limitAdmittedNativeDiagnosticDocuments,
  resolveInstalledTraceDecayBinary,
  traceDecayInitializationOptions,
  toLspDiagnosticSeverity,
} from "../src/nativeDiagnostics.js";

test("serializes bounded native diagnostics without raw diagnostic data", () => {
  const payload = createNativeDiagnosticsPayload(
    "file:///workspace/src/example.ts",
    7,
    [
      {
        range: {
          start: { line: 1, character: 2 },
          end: { line: 1, character: 6 },
        },
        severity: 1,
        code: "no-explicit-any",
        source: "eslint",
        message: "Unexpected any.",
        data: {
          ruleId: "@typescript-eslint/no-explicit-any",
          category: "style",
          text: "const secret = 'must not cross the bridge'",
        },
      },
    ],
  );

  assert.deepEqual(payload, {
    uri: "file:///workspace/src/example.ts",
    version: 7,
    diagnostics: [
      {
        range: {
          start: { line: 1, character: 2 },
          end: { line: 1, character: 6 },
        },
        severity: 1,
        code: "no-explicit-any",
        source: "eslint",
        message: "Unexpected any.",
        data: {
          category: "style",
          ruleId: "@typescript-eslint/no-explicit-any",
        },
      },
    ],
  });
});

test("caps diagnostic count and message size", () => {
  const payload = createNativeDiagnosticsPayload(
    "file:///workspace/src/example.ts",
    8,
    Array.from({ length: MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT + 1 }, (_, index) => ({
      range: {
        start: { line: index, character: 0 },
        end: { line: index, character: 1 },
      },
      message: "🦀".repeat(MAX_NATIVE_DIAGNOSTIC_MESSAGE_BYTES),
    })),
  );

  assert.equal(payload.diagnostics.length, MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT);
  assert.ok(
    payload.diagnostics.every(
      (diagnostic) =>
        Buffer.byteLength(diagnostic.message, "utf8") <=
        MAX_NATIVE_DIAGNOSTIC_MESSAGE_BYTES,
    ),
  );
});

test("prioritizes admitted documents before the notification cap", () => {
  const documents = [
    ...Array.from(
      { length: MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT },
      (_, index) => `file:///outside/${index}.ts`,
    ),
    "file:///workspace/first.ts",
    "file:///workspace/second.ts",
  ];

  assert.deepEqual(
    limitAdmittedNativeDiagnosticDocuments(
      documents,
      (document) => document.startsWith("file:///workspace/"),
    ),
    ["file:///workspace/first.ts", "file:///workspace/second.ts"],
  );
});

test("batches startup documents instead of dropping the tail", () => {
  const documents = Array.from(
    { length: MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT + 3 },
    (_, index) => `file:///workspace/${index}.ts`,
  );

  assert.deepEqual(
    batchAdmittedNativeDiagnosticDocuments(documents, () => true),
    [
      documents.slice(0, MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT),
      documents.slice(MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT),
    ],
  );
});

test("declares Cursor-native mode and TraceDecay context support", () => {
  assert.deepEqual(traceDecayInitializationOptions(), {
    tracedecay: {
      context: true,
      mode: "cursor-native",
    },
  });
});

test("returns typed unavailable state for multi-root workspaces", () => {
  assert.deepEqual(admitNativeWorkspace(true, ["file:///one", "file:///two"]), {
    state: "unavailable",
    reason: "workspace_root_count",
    expectedRootCount: 1,
    actualRootCount: 2,
  });
});

test("converts VS Code diagnostic severities to LSP severities", () => {
  assert.deepEqual(
    [0, 1, 2, 3, 99].map(toLspDiagnosticSeverity),
    [1, 2, 3, 4, undefined],
  );
});

test("prefers configured and packaged TraceDecay binaries", () => {
  assert.equal(
    resolveInstalledTraceDecayBinary(
      "/configured/tracedecay",
      "/environment/tracedecay",
      "/packaged/tracedecay",
    ),
    "/configured/tracedecay",
  );
  assert.equal(
    resolveInstalledTraceDecayBinary(
      "",
      "/environment/tracedecay",
      "/packaged/tracedecay",
    ),
    "/environment/tracedecay",
  );
  assert.equal(
    resolveInstalledTraceDecayBinary("", "", "/packaged/tracedecay"),
    "/packaged/tracedecay",
  );
});
