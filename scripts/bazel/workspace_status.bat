@echo off
rem Windows cannot execute workspace_status.sh directly, so hand it to the
rem Bash that runs genrules (BAZEL_SH), or Git Bash from PATH.
if defined BAZEL_SH (
  "%BAZEL_SH%" "%~dp0workspace_status.sh"
) else (
  bash "%~dp0workspace_status.sh"
)
