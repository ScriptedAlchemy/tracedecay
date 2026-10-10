"""An opt-in test over the SHA-pinned official Turso native SDK wheel."""

def _test_impl(ctx):
    marker = [file for file in ctx.files.wheel if file.path.endswith("turso/__init__.py")][0]
    wheel_root = marker.short_path[3:][:-len("turso/__init__.py")]
    script_path = ctx.workspace_name + "/" + ctx.file.script.short_path
    launcher = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(
        output = launcher,
        content = "#!/bin/sh\nset -eu\nexport PYTHONPATH=\"${TEST_SRCDIR}/%s\"\nexec \"${TURSO_PYTHON:-python3.11}\" \"${TEST_SRCDIR}/%s\" \"$@\"\n" % (wheel_root, script_path),
        is_executable = True,
    )
    return [DefaultInfo(executable = launcher, runfiles = ctx.runfiles(files = [ctx.file.script] + ctx.files.wheel + ctx.files.data))]

turso_python_test = rule(
    implementation = _test_impl,
    test = True,
    attrs = {
        "script": attr.label(allow_single_file = True, mandatory = True),
        "wheel": attr.label(mandatory = True),
        "data": attr.label_list(allow_files = True),
    },
)
