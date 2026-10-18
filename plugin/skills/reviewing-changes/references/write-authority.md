# Write authority after a review

A review reads; it grants no write authority. Generation-bound Git reads do not
authorize index or ref writes. TraceDecay Git preview and apply
(`tracedecay_git_preview`, then `tracedecay_git_apply`) consume the exact
authority the preview returned. Feedback advisory handles are read-only
diagnostics and never permission to post or resolve GitHub comments.

Branch-stack integration requires its frozen snapshot and an approved
application identity (`tracedecay_approve_native_integration`,
`tracedecay_apply_native_integration`, `tracedecay_cancel_native_integration`).
An ordinary review does not authorize integration.

Worktree cleanup consumes inventory, inspection, confirmation, removal
(`tracedecay_worktree_cleanup_remove`), and reconciliation identities, and it
does not delete a branch. Preserve peer ownership and reconcile typed outcomes
rather than reconstructing a target from its name.
