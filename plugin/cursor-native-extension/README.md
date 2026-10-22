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
exception. `pnpm run build` regenerates it from TypeScript; `pnpm run
check:embedded` performs a byte-for-byte drift check. `dist/` remains ignored.

Run the selector behavior check with the installed Cursor editor CLI. Set
`CURSOR_EDITOR` to that executable, rather than an agent CLI shim. From this
directory, use a no-op development extension so the selector test does not start
the TraceDecay bridge:

```sh
selector_test_root="$(mktemp -d)"
mkdir -p "$selector_test_root"/{development,workspace,profile,extensions}
printf '%s' '{"name":"selector-test","publisher":"tracedecay","version":"0.0.0","engines":{"vscode":"^1.125.0"},"main":"extension.js"}' > "$selector_test_root/development/package.json"
printf '%s' 'exports.activate = () => {};' > "$selector_test_root/development/extension.js"
"$CURSOR_EDITOR" --wait --verbose --skip-onboarding --skip-welcome \
  --user-data-dir="$selector_test_root/profile" \
  --extensions-dir="$selector_test_root/extensions" \
  --extensionDevelopmentPath="$selector_test_root/development" \
  --extensionTestsPath="$PWD/test/nativeDiagnostics.selector.extensionTests.cjs" \
  "$selector_test_root/workspace"
rm -rf "$selector_test_root"
```

The test uses temporary files and profiles but inherits `HOME`. It checks the
production selector through the language-client converter and Cursor matcher;
it does not validate daemon synchronization or native diagnostic ingestion.
