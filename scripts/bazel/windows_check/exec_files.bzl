"""Re-exports a target's files from the exec configuration."""

def _exec_files_impl(ctx):
    return [DefaultInfo(files = ctx.attr.src[DefaultInfo].files)]

exec_files = rule(
    implementation = _exec_files_impl,
    attrs = {"src": attr.label(cfg = "exec", mandatory = True)},
)
