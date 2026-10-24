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
  type NativeDiagnosticInput,
} from "../src/nativeDiagnostics.js";

function nativeDiagnostic(source?: string, index = 0): NativeDiagnosticInput {
  return {
    range: {
      start: { line: index, character: 0 },
      end: { line: index, character: 1 },
    },
    source,
    message: `Finding ${index}`,
  };
}

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

test("own publications do not displace a retained native finding", () => {
  const uri = "file:///workspace/src/example.ts";
  const native = nativeDiagnostic("typescript");
  const before = createNativeDiagnosticsPayload(uri, 8, [native]);
  const after = createNativeDiagnosticsPayload(uri, 8, [
    ...Array.from({ length: MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT }, (_, index) =>
      nativeDiagnostic("tracedecay", index),
    ),
    native,
  ]);

  assert.deepEqual(after, before);
});

test("caps eligible native findings in their original interleaved order", () => {
  const native = Array.from(
    { length: MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT + 1 },
    (_, index) => nativeDiagnostic(index % 2 === 0 ? "typescript" : "eslint", index),
  );
  const payload = createNativeDiagnosticsPayload(
    "file:///workspace/src/example.ts",
    9,
    native.flatMap((diagnostic, index) => [
      nativeDiagnostic("TraceDecay-CI", index),
      diagnostic,
    ]),
  );

  assert.deepEqual(
    payload.diagnostics.map(({ source, message }) => ({ source, message })),
    native
      .slice(0, MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT)
      .map(({ source, message }) => ({ source, message })),
  );
});

test("matches the receiver's source-prefix ownership without inspecting messages", () => {
  const ownedSources = [
    "tracedecay",
    "TRACEDECAY",
    "TrAcEdEcAy-github",
    "tracedecay-ci",
    "tracedecay-proximity",
    "tracedecayOther",
  ];
  const nativeSources = [undefined, "", "typescript", "trace", "x-tracedecay", " tracedecay"];
  const payload = createNativeDiagnosticsPayload(
    "file:///workspace/src/example.ts",
    10,
    [...ownedSources, ...nativeSources].map((source) => ({
      ...nativeDiagnostic(source),
      message: "tracedecay appears in this native diagnostic message",
    })),
  );

  assert.deepEqual(
    payload.diagnostics.map(({ source }) => source),
    nativeSources.map((source) => source || "cursor"),
  );
});

test("empty and own-only inputs still publish an explicit native clear", () => {
  const uri = "file:///workspace/src/example.ts";
  for (const diagnostics of [[], [nativeDiagnostic("TraceDecay-CI")]]) {
    assert.deepEqual(createNativeDiagnosticsPayload(uri, 11, diagnostics), {
      uri,
      version: 11,
      diagnostics: [],
    });
  }
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
