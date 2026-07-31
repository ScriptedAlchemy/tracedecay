use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::Instant;

use super::MEASURED_REPETITIONS;
use super::artifact::command_output;
use super::model::{NoOpTotals, RawPhaseSample};

pub(super) fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
}

pub(super) fn ticks_to_ms(ticks: u64, ticks_per_second: u64) -> f64 {
    ticks as f64 * 1_000.0 / ticks_per_second as f64
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct PhaseAggregate {
    pub(super) cpu_ticks: u64,
    pub(super) process_write_bytes: u64,
    pub(super) database_storage_growth_bytes: u64,
    pub(super) peak_rss_kib: u64,
}

pub(super) fn aggregate_samples(samples: &[RawPhaseSample]) -> PhaseAggregate {
    samples.iter().fold(
        PhaseAggregate {
            cpu_ticks: 0,
            process_write_bytes: 0,
            database_storage_growth_bytes: 0,
            peak_rss_kib: 0,
        },
        |mut aggregate, sample| {
            aggregate.cpu_ticks += sample.cpu_ticks;
            aggregate.process_write_bytes += sample.process_write_bytes;
            aggregate.database_storage_growth_bytes += sample.database_storage_growth_bytes;
            aggregate.peak_rss_kib = aggregate.peak_rss_kib.max(sample.peak_rss_kib);
            aggregate
        },
    )
}

pub(super) fn validate_no_op_invariants(
    samples: &[RawPhaseSample],
    observation_count_delta: i64,
    totals: &NoOpTotals,
) -> Result<(), String> {
    if !totals.is_zero() {
        return Err("no-op coordinator reported durable work".to_string());
    }
    validate_no_op_samples(samples, observation_count_delta, 0)
}

pub(super) fn validate_no_op_samples(
    samples: &[RawPhaseSample],
    observation_count_delta: i64,
    replayed_observations: usize,
) -> Result<(), String> {
    if samples.len() != MEASURED_REPETITIONS {
        return Err(format!(
            "expected {MEASURED_REPETITIONS} no-op samples, found {}",
            samples.len()
        ));
    }
    if observation_count_delta != 0 {
        return Err(format!(
            "no-op observation count changed by {observation_count_delta}"
        ));
    }
    for (expected_repetition, sample) in samples.iter().enumerate() {
        if sample.repetition != expected_repetition {
            return Err(format!(
                "no-op sample order expected repetition {expected_repetition}, found {}",
                sample.repetition
            ));
        }
        if sample.process_write_bytes != 0 {
            return Err(format!(
                "no-op repetition {} wrote {} process bytes",
                sample.repetition, sample.process_write_bytes
            ));
        }
        if sample.database_storage_growth_bytes != 0 {
            return Err(format!(
                "no-op repetition {} grew database storage by {} bytes",
                sample.repetition, sample.database_storage_growth_bytes
            ));
        }
        if sample.replayed_observations != replayed_observations {
            return Err(format!(
                "no-op repetition {} replayed {} observations instead of {replayed_observations}",
                sample.repetition, sample.replayed_observations,
            ));
        }
    }
    Ok(())
}

pub(super) fn process_cpu_ticks() -> u64 {
    let stat = fs::read_to_string("/proc/self/stat").expect("read process CPU counters");
    parse_proc_stat_cpu_ticks(&stat).expect("parse process CPU counters")
}

pub(super) fn parse_proc_stat_cpu_ticks(stat: &str) -> Result<u64, String> {
    let after_name = stat
        .rfind(')')
        .and_then(|index| stat.get(index + 2..))
        .ok_or_else(|| "missing process-name terminator in /proc/self/stat".to_string())?;
    let fields = after_name.split_whitespace().collect::<Vec<_>>();
    let user = fields
        .get(11)
        .ok_or_else(|| "missing utime in /proc/self/stat".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("parse process user ticks: {error}"))?;
    let system = fields
        .get(12)
        .ok_or_else(|| "missing stime in /proc/self/stat".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("parse process system ticks: {error}"))?;
    user.checked_add(system)
        .ok_or_else(|| "process CPU tick total overflowed u64".to_string())
}

pub(super) fn process_write_bytes() -> u64 {
    proc_value("/proc/self/io", "write_bytes:")
}

pub(super) fn reset_peak_rss() {
    write_clear_refs().expect("reset process peak RSS");
}

pub(super) fn process_peak_rss_kib() -> u64 {
    proc_value("/proc/self/status", "VmHWM:")
}

pub(super) fn memory_total_kib() -> u64 {
    proc_value("/proc/meminfo", "MemTotal:")
}

fn proc_value(path: &str, key: &str) -> u64 {
    let contents = fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    parse_proc_value(&contents, key).unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

pub(super) fn parse_proc_value(contents: &str, key: &str) -> Result<u64, String> {
    contents
        .lines()
        .find_map(|line| {
            let (candidate, value) = line.split_once(':')?;
            if candidate.trim() != key.trim_end_matches(':') {
                return None;
            }
            value.split_whitespace().next()?.parse::<u64>().ok()
        })
        .ok_or_else(|| format!("missing or invalid {key}"))
}

pub(super) fn cpu_identity() -> String {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").expect("read CPU identity");
    parse_cpu_identity(&cpuinfo)
        .unwrap_or_else(|| format!("unknown Linux CPU ({})", std::env::consts::ARCH))
}

pub(super) fn parse_cpu_identity(cpuinfo: &str) -> Option<String> {
    const KEYS: &[&str] = &[
        "model name",
        "hardware",
        "cpu",
        "uarch",
        "processor",
        "cpu model",
        "machine",
    ];
    KEYS.iter().find_map(|wanted| {
        cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim().eq_ignore_ascii_case(wanted) && !value.trim().is_empty())
                .then(|| value.trim().to_string())
        })
    })
}

fn write_clear_refs() -> std::io::Result<()> {
    let mut clear_refs = OpenOptions::new()
        .write(true)
        .open("/proc/self/clear_refs")?;
    clear_refs.write_all(b"5\n")
}

pub(super) fn parse_clock_ticks_per_second(output: &str) -> Result<u64, String> {
    let ticks = output
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("parse getconf CLK_TCK: {error}"))?;
    if ticks == 0 {
        return Err("getconf CLK_TCK returned zero".to_string());
    }
    Ok(ticks)
}

pub(super) fn preflight_platform() -> u64 {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "PR5 benchmark contract requires Linux"
    );
    for path in [
        "/proc/self/stat",
        "/proc/self/io",
        "/proc/self/status",
        "/proc/meminfo",
        "/proc/cpuinfo",
    ] {
        fs::File::open(path).unwrap_or_else(|error| {
            panic!("PR5 benchmark contract requires readable {path}: {error}")
        });
    }
    write_clear_refs().unwrap_or_else(|error| {
        panic!(
            "PR5 benchmark contract requires writable /proc/self/clear_refs with value 5: {error}"
        )
    });
    parse_clock_ticks_per_second(&command_output("getconf", &["CLK_TCK"]))
        .expect("PR5 benchmark contract requires nonzero getconf CLK_TCK")
}
