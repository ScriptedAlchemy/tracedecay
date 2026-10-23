//! Under a 6 GiB resident-memory limit, the shipped daemon cold-indexes and
//! repeatedly refreshes a heavy corpus without refusing work it has room for
//! and without its resident set crossing the limit.
//!
//! Its own binary: one daemon indexing a large corpus is the whole test.

#![cfg(all(target_os = "linux", not(feature = "alloc-jemalloc")))]

#[path = "../../tracedecay/tests/common/mod.rs"]
mod common;

use std::ffi::CString;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tempfile::TempDir;

use common::fixture::{git_capture, git_run};
use common::{
    canonical_existing_path, initialize_tracedecay_cli_project, spawn_tracedecay_daemon_logged,
    tool_json_structured_content, tracedecay_command_with_home,
};

const MIB: u64 = 1024 * 1024;
const LIMIT_BYTES: u64 = 6 * 1024 * MIB;
const REFRESHES: usize = 4;
const FILES: usize = 300;
const FUNCTIONS_PER_FILE: usize = 160;
const INDEX_TIMEOUT: Duration = Duration::from_secs(900);
const STATUS_POLL: Duration = Duration::from_secs(5);
const PERF_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const PERF_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const TEMPORARY_CGROUP_RESOURCES: [&str; 8] = [
    "cpu.max",
    "cpu.stat",
    "cpu.pressure",
    "cpuset.cpus.effective",
    "memory.current",
    "memory.max",
    "memory.events",
    "memory.pressure",
];

struct TemporaryWorkerResources {
    output: fs::File,
    daemon_pid: u32,
    started: Instant,
}

fn record_raw_resource(output: &mut fs::File, path: &Path) -> std::io::Result<String> {
    let result = fs::read_to_string(path);
    writeln!(output, "resource_begin path={}", path.display()).expect("resource artifact");
    match &result {
        Ok(raw) if path.ends_with("status") => {
            for field in [
                "Threads:",
                "Cpus_allowed_list:",
                "Mems_allowed_list:",
                "VmRSS:",
                "VmHWM:",
            ] {
                match raw.lines().find(|line| line.starts_with(field)) {
                    Some(line) => writeln!(output, "{line}"),
                    None => writeln!(output, "unavailable field={field}"),
                }
                .expect("resource artifact");
            }
        }
        Ok(raw) if path == Path::new("/proc/cpuinfo") => {
            for field in ["processor", "vendor_id", "model name", "cpu MHz"] {
                match raw.lines().find(|line| line.starts_with(field)) {
                    Some(line) => writeln!(output, "{line}"),
                    None => writeln!(output, "unavailable field={field}"),
                }
                .expect("resource artifact");
            }
        }
        Ok(raw) if path == Path::new("/proc/stat") => {
            match raw.lines().find(|line| line.starts_with("cpu ")) {
                Some(line) => writeln!(output, "{line}"),
                None => writeln!(output, "unavailable aggregate cpu line"),
            }
            .expect("resource artifact");
        }
        Ok(raw) => writeln!(output, "{raw}").expect("resource artifact"),
        Err(error) => writeln!(output, "unavailable error={error:?}").expect("resource artifact"),
    }
    writeln!(output, "resource_end path={}", path.display()).expect("resource artifact");
    result
}

impl TemporaryWorkerResources {
    fn sample(&mut self, event: &str) {
        let sample_started = Instant::now();
        writeln!(
            self.output,
            "TEMPORARY_DIAGNOSTIC_MUST_NOT_MERGE sample_begin event={event} unix_time={:?} elapsed={:?} test_pid={} daemon_pid={} available_parallelism={:?}",
            SystemTime::now().duration_since(UNIX_EPOCH),
            self.started.elapsed(),
            std::process::id(),
            self.daemon_pid,
            std::thread::available_parallelism(),
        )
        .expect("resource artifact");
        let clock_ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if clock_ticks > 0 {
            writeln!(
                self.output,
                "proc_stat_clock_ticks_per_second={clock_ticks}"
            )
            .expect("resource artifact");
        } else {
            writeln!(self.output, "proc_stat_clock_ticks_per_second=unavailable sysconf_result={clock_ticks}; proc_stat_cpu_times_are_raw_ticks")
                .expect("resource artifact");
        }
        for path in [
            "/proc/loadavg",
            "/proc/stat",
            "/proc/pressure/cpu",
            "/proc/pressure/io",
            "/proc/pressure/memory",
        ] {
            let _ = record_raw_resource(&mut self.output, Path::new(path));
        }
        let root = Path::new("/sys/fs/cgroup");
        writeln!(
            self.output,
            "cgroup_scope=visible_namespace_root effective_daemon_quota=unproven"
        )
        .expect("resource artifact");
        for name in TEMPORARY_CGROUP_RESOURCES {
            let _ = record_raw_resource(&mut self.output, &root.join(name));
        }
        let threads = PathBuf::from(format!("/proc/{}/task", self.daemon_pid));
        match fs::read_dir(&threads) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            for name in ["comm", "stat", "schedstat"] {
                                let _ =
                                    record_raw_resource(&mut self.output, &entry.path().join(name));
                            }
                        }
                        Err(error) => {
                            writeln!(self.output, "thread_entry_unavailable error={error:?}")
                                .expect("resource artifact")
                        }
                    }
                }
            }
            Err(error) => writeln!(self.output, "threads_unavailable error={error:?}")
                .expect("resource artifact"),
        }
        for pid in [std::process::id(), self.daemon_pid] {
            for name in ["stat", "schedstat", "io", "status"] {
                let _ = record_raw_resource(
                    &mut self.output,
                    &PathBuf::from(format!("/proc/{pid}/{name}")),
                );
            }
            let membership = record_raw_resource(
                &mut self.output,
                &PathBuf::from(format!("/proc/{pid}/cgroup")),
            );
            let Ok(membership) = membership else {
                continue;
            };
            let Some(relative) = membership
                .lines()
                .find_map(|line| line.strip_prefix("0::/"))
            else {
                writeln!(
                    self.output,
                    "cgroup_v2_unavailable pid={pid} reason=no_unified_membership"
                )
                .expect("resource artifact");
                continue;
            };
            if !Path::new(relative)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                writeln!(
                    self.output,
                    "cgroup_v2_unavailable pid={pid} reason=membership_outside_visible_root"
                )
                .expect("resource artifact");
                continue;
            }
            let mut directory = root.join(relative);
            for depth in 0..32 {
                if directory == root {
                    break;
                }
                for name in TEMPORARY_CGROUP_RESOURCES {
                    let _ = record_raw_resource(&mut self.output, &directory.join(name));
                }
                if depth == 31 {
                    writeln!(
                        self.output,
                        "cgroup_v2_ancestors_truncated pid={pid} limit=32"
                    )
                    .expect("resource artifact");
                    break;
                }
                directory.pop();
            }
        }
        writeln!(
            self.output,
            "sample_end event={event} elapsed={:?} observation_elapsed={:?}\n",
            self.started.elapsed(),
            sample_started.elapsed()
        )
        .expect("resource artifact");
        self.output.flush().expect("flush resource artifact");
    }
}

struct CpuSample {
    child: Option<Child>,
    control: fs::File,
    ack: fs::File,
    _fifos: TempDir,
    log: fs::File,
    perf: PathBuf,
    profile: PathBuf,
    stderr: PathBuf,
    daemon_pid: u32,
    daemon_start: u64,
    profiler_pid: u32,
    started: Instant,
}

fn daemon_starttime(pid: u32) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| io::Error::other("invalid daemon proc stat"))
}

fn require_daemon(pid: u32, start: u64) -> Result<(), String> {
    if daemon_starttime(pid).map_err(|error| error.to_string())? == start {
        Ok(())
    } else {
        Err(format!("daemon pid {pid} changed starttime from {start}"))
    }
}

fn prepare_cpu_profiler(out: &Path, daemon_pid: u32, daemon_start: u64) -> Result<PathBuf, String> {
    let perf = PathBuf::from(
        std::env::var_os("TRACEDECAY_PERF_BIN").ok_or("TRACEDECAY_PERF_BIN is required")?,
    );
    let readelf = PathBuf::from(
        std::env::var_os("TRACEDECAY_READELF").ok_or("TRACEDECAY_READELF is required")?,
    );
    let declared_binary = fs::canonicalize(
        std::env::var_os("TRACEDECAY_TEST_BIN").ok_or("TRACEDECAY_TEST_BIN is required")?,
    )
    .map_err(|error| error.to_string())?;
    let binary =
        fs::canonicalize(format!("/proc/{daemon_pid}/exe")).map_err(|error| error.to_string())?;
    if binary != declared_binary {
        return Err(format!(
            "daemon executable {} differs from test binary {}",
            binary.display(),
            declared_binary.display()
        ));
    }
    if !perf.is_absolute() || !readelf.is_absolute() {
        return Err("perf and readelf paths must be absolute".into());
    }
    let mut host = fs::File::create(out.join("heavy-refresh-profiler-host.txt"))
        .map_err(|error| error.to_string())?;
    writeln!(
        host,
        "perf={} readelf={} binary={} daemon_pid={daemon_pid} daemon_starttime={daemon_start}",
        perf.display(),
        readelf.display(),
        binary.display()
    )
    .map_err(|error| error.to_string())?;
    let help = Command::new(&perf)
        .args(["record", "-h"])
        .output()
        .map_err(|error| error.to_string())?;
    let help_status = help.status;
    let help_text = format!(
        "status={}\nstdout:\n{}\nstderr:\n{}",
        help_status,
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    fs::write(out.join("heavy-refresh-perf-record-help.txt"), &help_text)
        .map_err(|error| error.to_string())?;
    let help_output = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    if !help_output.contains("--control") || !help_output.contains("--delay") {
        return Err(format!(
            "installed perf record -h lacks --control or --delay (exit {help_status})"
        ));
    }
    let inspection = Command::new(readelf)
        .args(["--wide", "--sections", "--symbols", "--notes"])
        .arg(&binary)
        .output()
        .map_err(|error| error.to_string())?;
    if !inspection.status.success() {
        return Err(format!("readelf failed: {}", inspection.status));
    }
    let elf = String::from_utf8_lossy(&inspection.stdout);
    fs::write(out.join("heavy-refresh-daemon-elf.txt"), elf.as_bytes())
        .map_err(|error| error.to_string())?;
    let section_size = |name: &str| {
        elf.lines()
            .find(|line| line.split_whitespace().any(|field| field == name))
            .and_then(|line| line.split(name).nth(1))
            .and_then(|tail| tail.split_whitespace().nth(3))
            .and_then(|size| u64::from_str_radix(size, 16).ok())
            .unwrap_or(0)
    };
    let (symtab, unwind) = (section_size(".symtab"), section_size(".eh_frame"));
    let mut in_symtab = false;
    let mut functions = 0;
    for line in elf.lines() {
        if line.starts_with("Symbol table '") {
            in_symtab = line.contains("'.symtab'");
        } else if in_symtab && line.contains(" FUNC ") {
            functions += 1;
        }
    }
    if symtab == 0 || unwind == 0 || functions == 0 {
        return Err(format!(
            "unusable daemon ELF attribution: symtab={symtab} eh_frame={unwind} functions={functions}"
        ));
    }
    let identity = elf
        .lines()
        .find_map(|line| line.trim().strip_prefix("Build ID: "))
        .map(str::to_owned)
        .or_else(|| {
            Command::new("sha256sum")
                .arg(&binary)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| {
                    String::from_utf8_lossy(&output.stdout)
                        .split_whitespace()
                        .next()
                        .map(str::to_owned)
                })
        })
        .ok_or("daemon ELF has no build ID or digest")?;
    writeln!(host, "binary_identity={identity} symtab_size={symtab} eh_frame_size={unwind} symtab_function_count={functions} separate_debug_reference={}", elf.contains(".gnu_debuglink") || elf.contains(".gnu_debugaltlink")).map_err(|error| error.to_string())?;
    for path in [
        PathBuf::from("/proc/sys/kernel/random/boot_id"),
        PathBuf::from("/proc/sys/kernel/hostname"),
        PathBuf::from("/sys/devices/system/cpu/online"),
    ] {
        let _ = record_raw_resource(&mut host, &path);
    }
    let _ = record_raw_resource(
        &mut host,
        &PathBuf::from(format!("/proc/{daemon_pid}/maps")),
    );
    Ok(perf)
}

fn append_log(path: &Path, text: &[u8]) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text)
}

fn create_fifo(path: &Path) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn open_fifo(path: &Path) -> io::Result<fs::File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

fn wait_ack(file: &mut fs::File, child: &mut Child) -> Result<(), String> {
    let deadline = Instant::now() + PERF_ACK_TIMEOUT;
    let mut bytes = Vec::new();
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("perf exited before control ack: {status}"));
        }
        let mut descriptor = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 50) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.to_string());
        }
        if ready == 0 {
            continue;
        }
        let mut chunk = [0; 64];
        match file.read(&mut chunk) {
            Ok(count) if count > 0 => {
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.contains(&b'\n') {
                    if String::from_utf8_lossy(&bytes).trim() == "ack" {
                        return Ok(());
                    }
                    return Err(format!(
                        "unexpected perf ack {:?}",
                        String::from_utf8_lossy(&bytes)
                    ));
                }
            }
            Ok(_) => {}
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("timed out waiting for perf control ack".into())
}

impl CpuSample {
    fn prepare(
        perf: &Path,
        daemon_pid: u32,
        daemon_start: u64,
        started: Instant,
        out: &Path,
    ) -> Result<Self, String> {
        require_daemon(daemon_pid, daemon_start)?;
        let fifos = TempDir::new().map_err(|error| error.to_string())?;
        let control_path = fifos.path().join("control");
        let ack_path = fifos.path().join("ack");
        create_fifo(&control_path).map_err(|error| error.to_string())?;
        create_fifo(&ack_path).map_err(|error| error.to_string())?;
        let control = open_fifo(&control_path).map_err(|error| error.to_string())?;
        let ack_file = open_fifo(&ack_path).map_err(|error| error.to_string())?;
        let profile = out.join("heavy-refresh-generation-2.perf.data");
        let stderr = out.join("heavy-refresh-generation-2.perf.stderr");
        let log = fs::File::create(out.join("heavy-refresh-generation-2.perf.log"))
            .map_err(|error| error.to_string())?;
        let stderr_file = fs::File::create(&stderr).map_err(|error| error.to_string())?;
        let pid = daemon_pid.to_string();
        let child = Command::new(perf)
            .env("DEBUGINFOD_URLS", "")
            .args([
                "record",
                "--no-buildid-cache",
                "-F",
                "49",
                "-e",
                "cpu-clock:u",
                "--call-graph",
                "dwarf,8192",
                "--inherit",
                "--delay=-1",
            ])
            .arg(format!(
                "--control=fifo:{},{}",
                control_path.display(),
                ack_path.display()
            ))
            .args(["-p", pid.as_str(), "-o"])
            .arg(&profile)
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .map_err(|error| format!("spawn perf for daemon {daemon_pid}: {error}"))?;
        let profiler_pid = child.id();
        let mut sample = Self {
            child: Some(child),
            control,
            ack: ack_file,
            _fifos: fifos,
            log,
            perf: perf.to_path_buf(),
            profile,
            stderr,
            daemon_pid,
            daemon_start,
            profiler_pid,
            started,
        };
        sample.command(b"disable\n")?;
        writeln!(sample.log, "daemon_pid={daemon_pid} daemon_starttime={daemon_start} profiler_pid={profiler_pid} attachment_ack=ack initial_state=disabled profile={}", sample.profile.display()).map_err(|error| error.to_string())?;
        for name in ["stat", "status", "io"] {
            let _ = record_raw_resource(
                &mut sample.log,
                &PathBuf::from(format!("/proc/{profiler_pid}/{name}")),
            );
        }
        Ok(sample)
    }

    fn command(&mut self, command: &[u8]) -> Result<(), String> {
        self.control
            .write_all(command)
            .map_err(|error| error.to_string())?;
        self.control.flush().map_err(|error| error.to_string())?;
        wait_ack(
            &mut self.ack,
            self.child.as_mut().ok_or("profiler already reaped")?,
        )
    }

    fn enable(&mut self) -> Result<(), String> {
        self.command(b"enable\n")?;
        writeln!(
            self.log,
            "enable_ack_monotonic={:?}",
            self.started.elapsed()
        )
        .map_err(|error| error.to_string())
    }

    fn stop_child(&mut self) -> Result<ExitStatus, String> {
        let pid = self.child.as_ref().ok_or("profiler already reaped")?.id();
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGINT);
        }
        let deadline = Instant::now() + PERF_STOP_TIMEOUT;
        loop {
            let child = self.child.as_mut().ok_or("profiler already reaped")?;
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child.take();
                    return Ok(status);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => {
                    let child = self.child.as_mut().ok_or("profiler already reaped")?;
                    let _ = child.kill();
                    let status = child.wait().map_err(|error| error.to_string())?;
                    self.child.take();
                    return Err(format!(
                        "perf missed stop deadline and was killed: {status}"
                    ));
                }
                Err(error) => {
                    let child = self.child.as_mut().ok_or("profiler already reaped")?;
                    let _ = child.kill();
                    let status = child.wait().map_err(|wait| format!("{error}; {wait}"))?;
                    self.child.take();
                    return Err(format!("perf wait failed and was killed: {status}"));
                }
            }
        }
    }

    fn finish(mut self) -> Result<(), String> {
        let disabled = self.command(b"disable\n");
        writeln!(
            self.log,
            "disable_ack={disabled:?} monotonic={:?}",
            self.started.elapsed()
        )
        .map_err(|error| error.to_string())?;
        let mut errors = disabled.err().into_iter().collect::<Vec<_>>();
        for name in ["stat", "status", "io"] {
            let _ = record_raw_resource(
                &mut self.log,
                &PathBuf::from(format!("/proc/{}/{name}", self.profiler_pid)),
            );
        }
        if let Err(error) = require_daemon(self.daemon_pid, self.daemon_start) {
            errors.push(error);
        }
        match self.stop_child() {
            Ok(status) => {
                writeln!(self.log, "profiler_exit={status}").map_err(|error| error.to_string())?;
                if !status.success() {
                    errors.push(format!("perf exited unsuccessfully: {status}"));
                }
            }
            Err(error) => errors.push(error),
        }
        if fs::metadata(&self.profile)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            == 0
        {
            errors.push("perf profile is missing or empty".into());
        }
        if errors.is_empty() {
            self.export()?;
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn export(&mut self) -> Result<(), String> {
        require_daemon(self.daemon_pid, self.daemon_start)?;
        let out = self
            .profile
            .parent()
            .ok_or("profile has no output directory")?;
        for (name, children) in [("self", "--no-children"), ("inclusive", "--children")] {
            let report = Command::new(&self.perf)
                .env("DEBUGINFOD_URLS", "")
                .args(["report", "--stdio", children, "--percent-limit", "0", "-i"])
                .arg(&self.profile)
                .output()
                .map_err(|error| error.to_string())?;
            fs::write(
                out.join(format!("heavy-refresh-generation-2-{name}.txt")),
                &report.stdout,
            )
            .map_err(|error| error.to_string())?;
            append_log(&self.stderr, &report.stderr).map_err(|error| error.to_string())?;
            if !report.status.success() {
                return Err(format!("perf report {name} failed: {}", report.status));
            }
        }
        let script = Command::new(&self.perf)
            .env("DEBUGINFOD_URLS", "")
            .args([
                "script",
                "-F",
                "comm,pid,tid,time,period,event,ip,sym,dso",
                "-i",
            ])
            .arg(&self.profile)
            .output()
            .map_err(|error| error.to_string())?;
        let samples_path = out.join("heavy-refresh-generation-2-samples.txt");
        fs::write(&samples_path, &script.stdout).map_err(|error| error.to_string())?;
        append_log(&self.stderr, &script.stderr).map_err(|error| error.to_string())?;
        if !script.status.success() {
            return Err(format!("perf script failed: {}", script.status));
        }
        let text = String::from_utf8_lossy(&script.stdout);
        let samples: Vec<_> = text
            .lines()
            .filter(|line| line.contains("cpu-clock:u"))
            .collect();
        if samples.is_empty() {
            return Err("perf script contains no cpu-clock:u samples".into());
        }
        let periods: u64 = samples
            .iter()
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                let event = fields
                    .iter()
                    .position(|field| field.starts_with("cpu-clock:u"))?;
                fields.get(event.checked_sub(1)?)?.parse().ok()
            })
            .sum();
        let unknown = text
            .lines()
            .filter(|line| line.contains("[unknown]") || line.trim_start().starts_with("0x"))
            .count();
        let stderr = fs::read_to_string(&self.stderr)
            .map_err(|error| error.to_string())?
            .to_ascii_lowercase();
        writeln!(self.log, "sample_records={} period_total={} unresolved_frame_lines={} lost_warning={} throttling_warning={} sample_export={}", samples.len(), periods, unknown, stderr.contains("lost"), stderr.contains("throttl"), samples_path.display()).map_err(|error| error.to_string())?;
        Ok(())
    }
}

impl Drop for CpuSample {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = writeln!(
                self.log,
                "cleanup=drop monotonic={:?}",
                self.started.elapsed()
            );
            let _ = self.stop_child();
        }
    }
}

/// Every function of every file changes with `edit`, so each refresh
/// reparses the whole corpus.
fn write_corpus(project: &Path, edit: usize) {
    let src = project.join("src");
    fs::create_dir_all(&src).expect("source directory");
    for file in 0..FILES {
        let mut source = String::new();
        for function in 0..FUNCTIONS_PER_FILE {
            writeln!(
                source,
                "pub fn heavy_{file:03}_{function:03}(a: u32, b: u32) -> u32 {{\n    \
                 let c = a.wrapping_mul({function}) + b + {edit};\n    \
                 if c > {file} {{ c - a }} else {{ b.wrapping_add(c) }}\n}}"
            )
            .expect("format source");
        }
        fs::write(src.join(format!("module_{file:03}.rs")), source).expect("write source");
    }
}

fn run_cli(home: &Path, project: &Path, args: &[&str]) -> Vec<u8> {
    let started = Instant::now();
    eprintln!("heavy_refresh cli_started args={args:?}");
    let output = tracedecay_command_with_home(home)
        .args(args)
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("tracedecay {args:?}: {error}"));
    eprintln!(
        "heavy_refresh cli_finished args={args:?} elapsed={:?} status={}",
        started.elapsed(),
        output.status
    );
    assert!(
        output.status.success(),
        "tracedecay {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn tool(home: &Path, project: &Path, name: &str, arguments: &Value) -> Value {
    let project_arg = project.to_string_lossy().into_owned();
    let stdout = run_cli(
        home,
        project,
        &[
            "tool",
            "--project",
            &project_arg,
            name,
            "--json",
            "--args",
            &arguments.to_string(),
        ],
    );
    tool_json_structured_content(&stdout)
}

fn status(home: &Path, project: &Path) -> Value {
    tool(
        home,
        project,
        "status",
        &json!({
            "format": "json",
            "include_branch_diagnostics": false,
            "include_storage_health": false,
            "include_session_ingest": false,
            "include_staleness": false,
        }),
    )
}

/// Waits until the index serves a complete generation of `revision` and
/// returns that generation's id.
fn wait_for_current(
    home: &Path,
    project: &Path,
    revision: &str,
    resources: &mut TemporaryWorkerResources,
) -> String {
    let started = Instant::now();
    let deadline = started + INDEX_TIMEOUT;
    loop {
        resources.sample("status_poll");
        let status = status(home, project);
        eprintln!(
            "heavy_refresh index_status revision={revision} elapsed={:?} status={status}",
            started.elapsed()
        );
        let worktree = &status["code_index_freshness"]["worktree"];
        if status["code_index_freshness"]["status"] == "current"
            && worktree["source_revision"] == revision
        {
            assert_eq!(worktree["coverage"], "complete", "{status}");
            return worktree["latest_generation_id"]
                .as_str()
                .unwrap_or_else(|| panic!("a current index names its generation: {status}"))
                .to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the index never served {revision}: {status}"
        );
        std::thread::sleep(STATUS_POLL);
    }
}

fn process_status_bytes(pid: u32, field: &str) -> u64 {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).expect("daemon /proc status");
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|rest| rest.trim().strip_suffix("kB"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("{field} missing from /proc/{pid}/status"));
    kib * 1024
}

/// The daemon's own measurement stays nominal and inside the limit, and its
/// peak resident set never crossed it.
fn assert_within_limit(home: &Path, project: &Path, pid: u32) {
    let status = status(home, project);
    let memory = &status["memory"];
    assert_eq!(memory["limit_bytes"], LIMIT_BYTES, "{memory}");
    assert_eq!(memory["status"], "nominal", "{memory}");
    let resident = memory["resident_bytes"]
        .as_u64()
        .unwrap_or_else(|| panic!("the daemon measures itself: {memory}"));
    assert!(resident < LIMIT_BYTES, "{memory}");
    let peak = process_status_bytes(pid, "VmHWM:");
    assert!(
        peak < LIMIT_BYTES,
        "the daemon's peak resident set {peak} B crossed the {LIMIT_BYTES} B limit"
    );
}

#[test]
fn repeated_heavy_refreshes_under_the_limit_are_admitted() {
    let started = Instant::now();
    eprintln!("heavy_refresh started");
    let home = TempDir::new().expect("isolated home");
    let home = canonical_existing_path(home.path());
    let workspace = TempDir::new().expect("project root");
    let project = canonical_existing_path(workspace.path()).join("heavy");
    fs::create_dir_all(&project).expect("project directory");
    write_corpus(&project, 0);
    git_run(&project, &["init", "--quiet", "-b", "main"]);
    git_run(&project, &["add", "."]);
    git_run(&project, &["commit", "--quiet", "-m", "heavy corpus"]);
    let cold_revision = git_capture(&project, &["rev-parse", "HEAD"]);

    eprintln!(
        "heavy_refresh corpus_written elapsed={:?}",
        started.elapsed()
    );
    let log = PathBuf::from(
        std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR")
            .expect("diagnostic run requires Bazel undeclared outputs"),
    )
    .join("heavy-refresh-daemon.log");
    eprintln!(
        "heavy_refresh daemon_starting elapsed={:?}",
        started.elapsed()
    );
    let daemon = spawn_tracedecay_daemon_logged(&home, &log, |command| {
        command
            .env(
                "TRACEDECAY_RESIDENT_MEMORY_LIMIT_BYTES",
                LIMIT_BYTES.to_string(),
            )
            .env("RUST_LOG", "info")
            .env_remove("TRACEDECAY_SPAN_TIMINGS");
    });
    let mut resources = TemporaryWorkerResources {
        output: fs::File::create(log.with_file_name("heavy-refresh-worker-resources.log"))
            .expect("temporary worker resource artifact"),
        daemon_pid: daemon.id(),
        started,
    };
    writeln!(
        resources.output,
        "cpu_identity_scope=first_visible_processor cpu_mhz_scope=instantaneous"
    )
    .expect("resource artifact");
    let _ = record_raw_resource(&mut resources.output, Path::new("/proc/cpuinfo"));
    resources.sample("daemon_ready");
    eprintln!("heavy_refresh daemon_ready elapsed={:?}", started.elapsed());
    let output_dir = log.parent().expect("daemon output directory");
    let daemon_start_ticks =
        daemon_starttime(daemon.id()).expect("record ready daemon process starttime");
    let perf_path = prepare_cpu_profiler(output_dir, daemon.id(), daemon_start_ticks)
        .unwrap_or_else(|error| panic!("CPU profiler preparation failed: {error}"));
    eprintln!("heavy_refresh init_started elapsed={:?}", started.elapsed());
    initialize_tracedecay_cli_project(&home, &project);
    eprintln!(
        "heavy_refresh init_finished elapsed={:?}",
        started.elapsed()
    );
    resources.sample("init_finished");
    let mut generation = wait_for_current(&home, &project, &cold_revision, &mut resources);
    resources.sample("cold_current");
    assert_within_limit(&home, &project, daemon.id());

    for edit in 1..=REFRESHES {
        let mut cpu_sample = (edit == 2).then(|| {
            CpuSample::prepare(
                &perf_path,
                daemon.id(),
                daemon_start_ticks,
                started,
                output_dir,
            )
            .unwrap_or_else(|error| panic!("CPU profiler attach failed: {error}"))
        });
        resources.sample(&format!("edit_{edit}_started"));
        if let Some(sample) = cpu_sample.as_mut() {
            sample
                .enable()
                .unwrap_or_else(|error| panic!("CPU profiler enable failed: {error}"));
        }
        eprintln!(
            "heavy_refresh edit_started edit={edit} elapsed={:?}",
            started.elapsed()
        );
        write_corpus(&project, edit);
        git_run(&project, &["commit", "--quiet", "-am", "edit every file"]);
        let revision = git_capture(&project, &["rev-parse", "HEAD"]);
        resources.sample(&format!("edit_{edit}_sync_started"));
        run_cli(&home, &project, &["sync"]);
        let refreshed = wait_for_current(&home, &project, &revision, &mut resources);
        resources.sample(&format!("edit_{edit}_current"));
        assert_ne!(
            refreshed, generation,
            "refresh {edit} published a generation"
        );
        generation = refreshed;
        assert_within_limit(&home, &project, daemon.id());
        if let Some(sample) = cpu_sample {
            sample
                .finish()
                .unwrap_or_else(|error| panic!("CPU profiler finish failed: {error}"));
        }
        eprintln!(
            "heavy_refresh edit_finished edit={edit} elapsed={:?}",
            started.elapsed()
        );
    }

    let log = fs::read_to_string(&log).expect("daemon log");
    assert_eq!(
        log.matches("code_index_generation_published").count(),
        1 + REFRESHES,
        "the log records every index the daemon published:\n{log}"
    );
    assert!(
        !log.contains("resident-memory admission")
            && !log.contains("code_index_text_projection_waiting_for_memory"),
        "the daemon refused work it had room for:\n{log}"
    );
    resources.sample("daemon_stopping");
    drop(daemon);
    eprintln!("heavy_refresh finished elapsed={:?}", started.elapsed());
}
