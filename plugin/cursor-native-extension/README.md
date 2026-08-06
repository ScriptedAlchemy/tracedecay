# TraceDecay Cursor Native Diagnostics

This Cursor desktop extension starts `tracedecay lsp bridge --stdio` through
`vscode-languageclient` and forwards bounded, workspace-scoped native
diagnostics to its `tracedecay/nativeDiagnostics` notification.
Untrusted or non-single-root workspaces resolve through a typed unavailable
state; multi-root workspaces do not start the bridge.

TraceDecay packages the built, unpacked extension into Cursor's host-component
lifecycle under `.cursor/extensions/tracedecay.cursor-native-0.0.0/`. The
receipt-backed Doctor check verifies the deployed `package.json` and
`dist/extension.js`. The deployed bundle receives the installed TraceDecay
binary path from that lifecycle; `tracedecay.binaryPath` and `TRACEDECAY_BIN`
remain explicit overrides.

`embedded/extension.js` is the sole checked-in compiled asset. The Rust host
bundle embeds it at compile time and the published crate must build with stock
Cargo without Node, so this generated file is an explicit offline-packaging
exception. `npm run build` regenerates it from TypeScript; `npm run
check:embedded` performs a byte-for-byte drift check. `dist/` remains ignored.
