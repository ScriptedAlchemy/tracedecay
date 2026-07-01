# Final Plan: macOS launchd Support for the TraceDecay Daemon

**Status:** Final implementation plan
**Target branch:** `main`
**Scope:** `src/daemon.rs`, focused tests, README / user guide / security docs
**Outcome:** macOS gets the same user-facing daemon service support Linux already
has: `tracedecay daemon install-service`, `uninstall-service`, `status`, and
post-update service refresh work for a per-user background daemon.

---

## 1. Goal

Make the existing Linux daemon-service workflow work on macOS with native
launchd:

```bash
tracedecay daemon install-service
tracedecay daemon status
tracedecay daemon uninstall-service
tracedecay update
```

Today macOS fails because the service layer always goes through
`systemd_user_service_path()` in `src/daemon.rs`, which hard-errors outside
Linux:

> daemon service install is currently supported on Linux systemd user services

After this change, macOS users can install TraceDecay as a per-user LaunchAgent
that starts at GUI login, restarts on failure, serves the same Unix socket
daemon as `tracedecay daemon run`, and is refreshed by `tracedecay update`.

**Linux parity means OS-managed daemon process parity.** The macOS service should
match what Linux systemd support provides today:

- write the service definition;
- start and enable it when requested;
- preserve a previously installed custom socket path during refresh;
- stop/disable/remove it during uninstall;
- report service path, socket reachability, and useful log/service commands;
- refresh the installed service after an update.

## 2. Non-goals

- **Windows service support.** Windows remains on the existing non-Unix fallback.
- **Auto-installing the daemon from `install --agent X`.** Linux does not do this
  either; users explicitly opt into the OS service with `daemon install-service`.
- **Persisted project scheduler registry.** The daemon scheduler is currently
  seeded when project clients connect and send a `DaemonHandshake`. This plan
  does not add a boot-time registry of projects to resume before any client has
  connected. That would be beyond Linux parity and should be a separate design.
- **Changing storage roots.** macOS continues to use the current TraceDecay
  `user_data_dir()` behavior (`~/.tracedecay` unless `TRACEDECAY_DATA_DIR` is
  set). Do not silently move daemon sockets or logs to
  `~/Library/Application Support`.

## 3. Existing Code Shape

The current service API is already narrow enough for a clean platform dispatch.

| Function | Current behavior |
|---|---|
| `install_service(spec, start)` | writes systemd unit, optionally `daemon-reload` + `enable --now` |
| `refresh_service(spec)` | rewrites systemd unit, `daemon-reload`, `enable`, `restart` |
| `refresh_installed_service(spec)` | skips missing unit, preserves installed socket path, refreshes |
| `uninstall_service(stop)` | optionally `disable --now`, removes unit, `daemon-reload` |
| `installed_service_socket_path()` | reads installed unit and parses `--socket` |
| `service_status(socket_path)` | prints service path, socket state, log command |

Every one of those paths currently depends on `systemd_user_service_path()`.
That is the right seam to replace with platform dispatch.

The daemon engine itself is already usable on macOS:

- `run_foreground_unix` binds a Unix socket and handles SIGTERM;
- `notify_hook_event` has a Unix implementation;
- the scheduler code is Unix-gated, not Linux-specific;
- client handshake/profile handling is independent of systemd.

## 4. Architecture Decision

Keep the public Rust API signature-compatible and add private platform service
helpers inside `src/daemon.rs`.

```text
Public API
  install_service
  refresh_service
  refresh_installed_service
  uninstall_service
  installed_service_socket_path
  service_status
        |
        v
ServiceRunner::current()
        |
        +-- Linux  -> systemd user service
        +-- macOS  -> launchd per-user LaunchAgent
        +-- other  -> existing unsupported-service error
```

Use an enum, not traits, because there are only two supported backends and the
implementation is private:

```rust
enum ServiceRunner {
    Systemd,
    Launchd,
}
```

The pure rendering/parsing helpers must remain unit-testable on any platform.
The process-control helpers are platform-gated and tested with fake command
runners where possible.

## 5. macOS launchd Behavior

Use modern launchd domain commands. Do **not** use legacy `launchctl load` /
`unload` for the implementation because they hide many errors and are explicitly
documented as legacy on current macOS.

### 5.1 LaunchAgent identity

```rust
const LAUNCHD_LABEL: &str = "com.tracedecay.daemon";
const LAUNCHD_PLIST_NAME: &str = "com.tracedecay.daemon.plist";
```

Paths:

| Item | macOS path |
|---|---|
| LaunchAgent plist | `~/Library/LaunchAgents/com.tracedecay.daemon.plist` |
| socket | `<user_data_dir>/daemon.sock` unless `--socket` overrides |
| stdout log | `<user_data_dir>/daemon.out.log` |
| stderr log | `<user_data_dir>/daemon.err.log` |

The plist path follows macOS convention. The socket/log paths follow existing
TraceDecay storage behavior for parity with the current daemon code.

### 5.2 launchctl domain helpers

Add:

```rust
#[cfg(target_os = "macos")]
fn launchd_domain() -> Result<String>; // "gui/<uid>"

#[cfg(target_os = "macos")]
fn launchd_service_target() -> Result<String>; // "gui/<uid>/com.tracedecay.daemon"

#[cfg(target_os = "macos")]
fn run_launchctl(args: &[&str]) -> Result<CommandOutput>;
```

`run_launchctl` should capture stdout/stderr and include both in errors. Keep the
shape close to `run_systemctl`, but return output for status checks.

Use `gui/<uid>` because this is a per-user LaunchAgent that should start at GUI
login. If a future headless/background-user mode is needed, that should be a
separate option.

### 5.3 Service-control mapping

| Operation | Linux systemd | macOS launchd |
|---|---|---|
| install with start | `daemon-reload`; `enable --now tracedecay.service` | write plist; `bootstrap gui/<uid> <plist>`; `enable gui/<uid>/com.tracedecay.daemon`; `kickstart -k gui/<uid>/com.tracedecay.daemon` |
| install with `--no-start` | write unit only | write plist only |
| refresh | write unit; `daemon-reload`; `enable`; `restart` | write plist; if loaded, `bootout gui/<uid>/com.tracedecay.daemon`; `bootstrap gui/<uid> <plist>`; `enable ...`; `kickstart -k ...` |
| uninstall with stop | `disable --now`; remove unit; `daemon-reload` | `bootout gui/<uid>/com.tracedecay.daemon` if loaded; `disable gui/<uid>/com.tracedecay.daemon`; remove plist |
| uninstall with `--no-stop` | remove unit only | remove plist only |
| status | unit path + socket + journald hint | plist path + socket + `launchctl print` / log hints |

Implementation details:

- Treat "not bootstrapped/not found" during `bootout` as non-fatal for uninstall
  and refresh, just like the Linux uninstall ignores failed `disable --now`.
- After install/refresh with start, verify either the socket becomes connectable
  briefly or `launchctl print <service-target>` succeeds. This catches command
  failures that otherwise appear only in logs.
- Do not call `enable` or `kickstart` for `--no-start`.

## 6. Plist Rendering

Add:

```rust
impl DaemonServiceSpec {
    pub fn render_launchd_plist(&self) -> Result<String>;
}
```

The plist:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.tracedecay.daemon</string>

  <key>ProgramArguments</key>
  <array>
    <string>{absolute_tracedecay_bin}</string>
    <string>daemon</string>
    <string>run</string>
    <string>--socket</string>
    <string>{socket_path}</string>
  </array>

  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{daemon_service_path_env(bin)}</string>
    <key>HOME</key>
    <string>{home}</string>
  </dict>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>

  <key>ThrottleInterval</key>
  <integer>2</integer>

  <key>StandardOutPath</key>
  <string>{user_data_dir}/daemon.out.log</string>

  <key>StandardErrorPath</key>
  <string>{user_data_dir}/daemon.err.log</string>
</dict>
</plist>
```

Renderer requirements:

- XML-escape `&`, `<`, `>`, `"`, and `'`.
- Require an absolute binary path for launchd. `which_tracedecay()` should already
  produce one in normal installs; error clearly if it does not.
- Include `TRACEDECAY_DATA_DIR` in `EnvironmentVariables` when it is set during
  install. This preserves custom profile roots across launchd restarts.
- Create the log/socket data directory before bootstrapping, because launchd can
  create log files but cannot create missing parent directories.
- Set LaunchAgent plist permissions explicitly after writing. Use at most `0644`
  and avoid group/world-writable files.

## 7. Plist Parsing

Add:

```rust
fn socket_path_from_launchd_plist(plist: &str) -> Option<PathBuf>;
```

Minimum acceptable parser:

1. find the `ProgramArguments` array;
2. collect `<string>...</string>` values in order;
3. XML-unescape those string values;
4. return the value after `--socket`, or the value from `--socket=...` if ever
   emitted in the future.

Do not return escaped XML text. Existing custom socket preservation depends on
this parser during `refresh_installed_service`.

If adding a small plist parsing dependency is acceptable, prefer a real plist
parser. If not, keep the ad hoc parser tightly scoped and heavily tested.

## 8. `src/daemon.rs` Changes

### 8.1 Platform path helpers

Replace internal calls to `systemd_user_service_path()` with:

```rust
fn service_unit_path() -> Result<PathBuf>;
```

Behavior:

- Linux: existing `~/.config/systemd/user/tracedecay.service`;
- macOS: `~/Library/LaunchAgents/com.tracedecay.daemon.plist`;
- other: service install unsupported.

Keep `systemd_user_service_path()` as a private Linux helper.

### 8.2 Render/parse dispatch

Add:

```rust
impl DaemonServiceSpec {
    fn render_unit(&self) -> Result<String>;
}

fn socket_path_from_unit_text(text: &str) -> Option<PathBuf>;
```

Linux dispatches to existing systemd helpers. macOS dispatches to the new plist
helpers.

### 8.3 ServiceRunner methods

```rust
impl ServiceRunner {
    fn current() -> Result<Self>;
    fn install(&self, service_path: &Path, start: bool, socket_path: &Path) -> Result<()>;
    fn refresh(&self, service_path: &Path, socket_path: &Path) -> Result<()>;
    fn uninstall(&self, service_path: &Path, stop: bool) -> Result<()>;
    fn log_hint(&self) -> String;
    fn service_detail_hint(&self) -> Option<String>;
}
```

Use `socket_path` only for optional post-start verification. Keep the public API
signatures unchanged.

### 8.4 Public function rewiring

Refactor:

- `install_service`
- `refresh_service`
- `refresh_installed_service`
- `write_service_unit`
- `installed_service_socket_path`
- `service_socket_path_from_unit_file`
- `uninstall_service`
- `service_status`

The public surface remains unchanged. Only internal platform dispatch changes.

### 8.5 Status output

Keep status stable and useful:

```text
service: /Users/you/Library/LaunchAgents/com.tracedecay.daemon.plist
socket: /Users/you/.tracedecay/daemon.sock (connectable)
service-detail: launchctl print gui/501/com.tracedecay.daemon
logs: tail -f "/Users/you/.tracedecay/daemon.err.log"
```

For Linux, keep the existing journald hint.

Do not make status depend on parsing unstable `launchctl print` output. It is
fine to include the command as a diagnostic hint. If the implementation probes
service load state, treat it as best-effort.

## 9. Docs

Update all docs that currently describe daemon support as Linux-only or absent:

- `README.md`
  - daemon debugging section: show Linux and macOS commands;
  - CLI reference: remove "Linux systemd" qualifier from `daemon install-service`;
  - mention macOS logs under `<user_data_dir>/daemon.err.log`.
- `docs/USER-GUIDE.md`
  - add macOS daemon setup under the install or keeping-fresh flow;
  - show install, status, uninstall.
- `SECURITY.md`
  - replace the current "No background daemon" statement with an accurate
    opt-in model: no daemon is installed by default, but users can explicitly
    install a per-user systemd/launchd service that runs with standard user
    privileges.

## 10. Tests

### 10.1 Ungated unit tests

These run on every platform:

- `render_launchd_plist_includes_label_program_arguments_socket_and_logs`
- `render_launchd_plist_escapes_xml_special_characters`
- `render_launchd_plist_includes_trace_decay_data_dir_when_set`
- `socket_path_from_launchd_plist_round_trips_rendered_socket`
- `socket_path_from_launchd_plist_unescapes_xml`
- `socket_path_from_launchd_plist_returns_none_for_malformed_input`
- `service_unit_path_unsupported_platform_error_mentions_service_install`
  if practical to test via helper injection.

### 10.2 Linux regression tests

Existing Linux tests must keep passing:

- `user_service_runs_daemon_with_socket_path`
- `refresh_service_rewrites_unit_and_restarts_daemon`
- `refresh_installed_service_skips_missing_unit`
- `refresh_installed_service_preserves_existing_socket_path`

If the systemd renderer signature changes to return `Result<String>`, update the
tests mechanically without changing expected Linux output.

### 10.3 macOS command tests without real launchd

Add tests around command planning/fake command runner, not real `launchctl`:

- install with start plans `bootstrap`, `enable`, `kickstart`;
- install with `--no-start` writes plist only;
- refresh preserves existing socket path and plans `bootout`, `bootstrap`,
  `enable`, `kickstart`;
- uninstall with stop plans `bootout`, `disable`, remove plist;
- uninstall with `--no-stop` removes plist only.

These should not require root, a GUI session, or a real LaunchAgent.

### 10.4 Optional ignored macOS smoke test

One ignored/manual test is acceptable, but it must avoid clobbering a user's real
daemon:

- use a test-only label like `com.tracedecay.daemon.test.<pid>`;
- write to a temporary plist path;
- use a temporary `TRACEDECAY_DATA_DIR`;
- always attempt cleanup with `bootout` and file removal.

Do not use `com.tracedecay.daemon` in ignored tests.

## 11. Risks and Decisions

| Risk | Decision |
|---|---|
| legacy `launchctl load/unload` masks failures | use `bootstrap/bootout/enable/kickstart` |
| plist parent/log parent missing | create LaunchAgents dir and data dir before bootstrap |
| plist rejected due to permissions | set plist permissions explicitly |
| custom `TRACEDECAY_DATA_DIR` lost under launchd | persist it into plist env when set |
| custom socket path lost on update | parse plist and preserve existing `--socket` during refresh |
| status overpromises service state | show socket state and diagnostic launchctl command; probe best-effort only |
| scheduler expectations after reboot | document Linux parity: daemon starts at login; project schedulers start after project handshake |
| Homebrew binary path changes | `tracedecay update` refreshes plist with current binary path |
| log growth | same operational class as journald, but file rotation is a follow-up |

## 12. Implementation Order

1. **Pure service dispatch refactor**
   - add `ServiceRunner`;
   - add `service_unit_path`;
   - add render/parse dispatch helpers;
   - keep Linux behavior identical;
   - run existing daemon tests.

2. **Add launchd plist renderer/parser**
   - add XML escape/unescape;
   - add data-dir/log path handling;
   - add ungated parser/renderer tests.

3. **Add macOS launchctl backend**
   - implement domain/target helpers;
   - implement `bootstrap`, `bootout`, `enable`, `disable`, `kickstart`;
   - create required directories and set plist permissions;
   - add fake-command tests.

4. **Wire refresh/uninstall/status**
   - preserve existing socket paths;
   - add macOS log and `launchctl print` hints;
   - keep public API and CLI unchanged.

5. **Docs**
   - README;
   - user guide;
   - SECURITY.md.

6. **Manual macOS verification**
   - run on a real macOS GUI session;
   - verify install/start/status/refresh/uninstall;
   - verify reboot/login start.

## 13. Verification

Automated:

```bash
cargo nextest run -p tracedecay daemon
cargo test -p tracedecay daemon_install_service_command_parses_socket_and_no_start
```

Manual macOS:

```bash
tracedecay daemon install-service
tracedecay daemon status
launchctl print "gui/$(id -u)/com.tracedecay.daemon"
tail -f ~/.tracedecay/daemon.err.log
tracedecay update
tracedecay daemon uninstall-service
```

Expected:

- plist exists at `~/Library/LaunchAgents/com.tracedecay.daemon.plist`;
- socket reports connectable after install/start;
- `launchctl print gui/$(id -u)/com.tracedecay.daemon` succeeds while installed;
- killing the daemon process causes launchd to restart it;
- after reboot and GUI login, launchd starts the daemon;
- `tracedecay update` refreshes plist binary path and keeps the installed socket
  path;
- uninstall removes the plist and the launchd job.

Scheduler verification, matching current Linux behavior:

1. enable scheduler config for a project;
2. connect a project client through the daemon once;
3. verify `event=scheduler_tick` and task logs in `daemon.err.log`;
4. restart the daemon and reconnect the project client;
5. verify scheduler starts again.

Do not require scheduler ticks immediately after reboot before any project
client has connected; that is not Linux parity.

## 14. Source References

| Topic | File:line |
|---|---|
| Linux-only service path gate | `src/daemon.rs:1715 systemd_user_service_path` |
| systemd unit renderer | `src/daemon.rs:154 render_systemd_user_unit` |
| public install/refresh/uninstall/status | `src/daemon.rs:455 / :466 / :474 / :540 / :560` |
| socket-path parser | `src/daemon.rs:523 socket_path_from_service_unit` |
| systemctl runner | `src/daemon.rs:1731 run_systemctl` |
| PATH env helper | `src/daemon.rs:177 daemon_service_path_env` |
| default socket path | `src/daemon.rs:237 default_socket_path` |
| foreground Unix daemon | `src/daemon.rs:988 run_foreground_unix` |
| scheduler starts from project server | `src/daemon.rs:1097 project_server` |
| scheduler config gate | `src/daemon.rs:1421 automation_scheduler_configured` |
| daemon action dispatch | `src/main.rs:742 Commands::Daemon` |
| post-update daemon refresh | `src/main.rs:413 refresh_daemon_service` |
| daemon CLI enum | `src/cli.rs:446 DaemonAction` |
| current security doc conflict | `SECURITY.md:103 No background daemon` |
