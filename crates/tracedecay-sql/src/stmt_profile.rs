//! TEMPORARY profiling patch (#3491), never committed: per-statement wall
//! time and VM steps, keyed by SQL text, written by `dump` when
//! `TD_SQL_PROFILE` names an output path.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

#[derive(Default)]
struct Entry {
    runs: u64,
    nanos: u128,
    vm_steps: u64,
    last_vm: u64,
}

fn table() -> Option<&'static Mutex<HashMap<String, Entry>>> {
    static TABLE: OnceLock<Option<Mutex<HashMap<String, Entry>>>> = OnceLock::new();
    TABLE
        .get_or_init(|| std::env::var_os("TD_SQL_PROFILE").map(|_| Mutex::default()))
        .as_ref()
}

pub fn enabled() -> bool {
    table().is_some()
}

/// Adds wall time spent stepping `sql`.
pub fn time(sql: &str, elapsed: Duration) {
    let Some(table) = table() else { return };
    let mut table = table.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = table.entry(sql.to_owned()).or_default();
    entry.nanos += elapsed.as_nanos();
}

/// Records one finished run of `sql` with the program's cumulative VM steps.
pub fn finish(sql: &str, cumulative_vm: u64) {
    let Some(table) = table() else { return };
    let mut table = table.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = table.entry(sql.to_owned()).or_default();
    entry.runs += 1;
    let delta = if cumulative_vm >= entry.last_vm {
        cumulative_vm - entry.last_vm
    } else {
        cumulative_vm
    };
    entry.vm_steps += delta;
    entry.last_vm = cumulative_vm;
}

/// Records one run with its own wall time and VM steps.
pub fn record(sql: &str, elapsed: Duration, vm_steps: u64) {
    let Some(table) = table() else { return };
    let mut table = table.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = table.entry(sql.to_owned()).or_default();
    entry.runs += 1;
    entry.nanos += elapsed.as_nanos();
    entry.vm_steps += vm_steps;
}

pub fn dump() {
    let (Some(table), Some(path)) = (table(), std::env::var_os("TD_SQL_PROFILE")) else {
        return;
    };
    let table = table.lock().unwrap_or_else(PoisonError::into_inner);
    let mut rows: Vec<_> = table.iter().collect();
    rows.sort_by(|a, b| b.1.nanos.cmp(&a.1.nanos));
    let mut out = String::from("wall_ms\truns\tvm_steps\tsql\n");
    for (sql, entry) in rows {
        out.push_str(&format!(
            "{:.1}\t{}\t{}\t{}\n",
            entry.nanos as f64 / 1e6,
            entry.runs,
            entry.vm_steps,
            sql.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    let _ = std::fs::write(path, out);
}

pub fn reset() {
    if let Some(table) = table() {
        table.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }
}
