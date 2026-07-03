# Error class → tracedecay tool

A lookup table mapping a build/type-error class to the cheapest tracedecay tool
that anchors it to the graph. Prefer parsing pasted output (`tracedecay_diagnose`)
over running a fresh toolchain (`tracedecay_diagnostics`) whenever you already
captured the compiler's stderr.

| Error class | Signal | Start with | Then |
|---|---|---|---|
| Pasted `cargo`/`clippy`/`rustc` stderr on hand | You already ran the build | `tracedecay_diagnose` (`cargo_output`, `include_callers`) | Inspect the mapped node with `tracedecay_body` |
| No fresh diagnostics yet | Need to run the toolchain | `tracedecay_diagnostics` (`scope: workspace\|package\|file`) | Narrow `scope` to `file`/`package` on re-check |
| Undefined / unresolved symbol | "cannot find `X`", "no method named" | `tracedecay_search` then `tracedecay_signature` | `tracedecay_context` when name guesses fail |
| Signature / arity / type mismatch | "expected N args", "expected type T" | `tracedecay_signature` on the callee | `tracedecay_callers` to fix every call site |
| Missing struct field / bad initializer | "missing field", "no field `f`" | `tracedecay_constructors` (struct-literal sites) | `tracedecay_field_sites` for read/write sites |
| Trait not implemented / bound not satisfied | "the trait bound `T: Tr` is not satisfied" | `tracedecay_impls` / `tracedecay_implementations` | `tracedecay_type_hierarchy` for the trait tree |
| Borrow / lifetime error | "does not live long enough", "borrowed" | `tracedecay_body` on the enclosing fn | `tracedecay_callees` to see what escapes |
| Rename left dangling references | Post-refactor "cannot find" cascade | `tracedecay_rename_preview` (edges) | `tracedecay_similar` for post-rename collisions |
| Import / module path broken | "unresolved import", "module not found" | `tracedecay_module_api` | `tracedecay_file_dependents` for the ripple |
| Risky fix on a hub symbol | A fix touches a widely-used node | `tracedecay_impact` (shallow first) | `tracedecay_affected` for the test set |

After applying the fix (via `tracedecay:editing-safely`), re-check with the
cheapest applicable path above, then verify behavior with
`tracedecay:assessing-impact`.
