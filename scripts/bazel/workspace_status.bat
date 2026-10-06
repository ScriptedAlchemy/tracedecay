@echo off
rem Windows twin of workspace_status.sh: Bazel runs the status command
rem directly, and a bare `bash` there can resolve to WSL instead of Git Bash.
set "SHA="
for /f "delims=" %%i in ('git rev-parse HEAD') do set "SHA=%%i"
if not defined SHA exit /b 1
set "DIRTY="
for /f "delims=" %%i in ('git status --porcelain --untracked-files^=normal') do set "DIRTY=.dirty"
echo STABLE_PRODUCT_GIT_SHA %SHA%%DIRTY%
