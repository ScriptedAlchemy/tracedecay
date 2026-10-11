//! Measurement-only per-statement work attribution for #3491 (PERF5). Off
//! unless a process installs an instruction counter with [`install`].

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Mutex, OnceLock, PoisonError},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Entry {
    pub executions: u64,
    pub instructions: u64,
    pub vm_steps: u64,
    pub changes: u64,
    pub bind_bytes: u64,
}

static COUNTER: OnceLock<fn() -> u64> = OnceLock::new();
static TOTALS: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);

thread_local! {
    static STACK: RefCell<Vec<(u64, u64, u64)>> = const { RefCell::new(Vec::new()) };
}

pub fn install(counter: fn() -> u64) {
    let _ = COUNTER.set(counter);
}

pub fn enabled() -> bool {
    COUNTER.get().is_some()
}

fn now() -> u64 {
    COUNTER.get().map_or(0, |counter| counter())
}

pub fn begin() {
    if enabled() {
        let start = now();
        STACK.with(|stack| stack.borrow_mut().push((start, 0, 0)));
    }
}

/// Adds bound-parameter bytes to the innermost open span.
pub fn note_bytes(bytes: u64) {
    if enabled() {
        STACK.with(|stack| {
            if let Some(top) = stack.borrow_mut().last_mut() {
                top.2 += bytes;
            }
        });
    }
}

/// Closes the innermost open span and charges its exclusive instructions.
pub fn end(sql: &str, entry: Entry) {
    if !enabled() {
        return;
    }
    let stop = now();
    let Some((exclusive, bytes)) = STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let (start, children, bytes) = stack.pop()?;
        let inclusive = stop.saturating_sub(start);
        if let Some(parent) = stack.last_mut() {
            parent.1 += inclusive;
        }
        Some((inclusive.saturating_sub(children), bytes))
    }) else {
        return;
    };
    let mut totals = TOTALS.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = totals
        .get_or_insert_with(HashMap::new)
        .entry(key(sql))
        .or_default();
    slot.executions += entry.executions;
    slot.instructions += exclusive;
    slot.vm_steps += entry.vm_steps;
    slot.changes += entry.changes;
    slot.bind_bytes += entry.bind_bytes + bytes;
}

pub fn report() -> Vec<(String, Entry)> {
    let totals = TOTALS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut rows: Vec<_> = totals
        .iter()
        .flat_map(HashMap::iter)
        .map(|(sql, entry)| (sql.clone(), *entry))
        .collect();
    rows.sort_by(|left, right| right.1.instructions.cmp(&left.1.instructions));
    rows
}

fn key(sql: &str) -> String {
    let collapsed = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(200).collect()
}

#[cfg(not(td_engine = "native"))]
pub(crate) fn trace_sqlite(connection: &rusqlite::Connection) {
    use rusqlite::trace::{TraceEvent, TraceEventCodes};

    fn record(event: TraceEvent<'_>) {
        static LAST_VM: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
        match event {
            TraceEvent::Stmt(_, sql) if !sql.starts_with("--") => begin(),
            TraceEvent::Profile(statement, _) => {
                let sql = statement.sql();
                let cumulative =
                    u64::try_from(statement.get_status(rusqlite::StatementStatus::VmStep))
                        .unwrap_or(0);
                let mut last = LAST_VM.lock().unwrap_or_else(PoisonError::into_inner);
                let previous = last
                    .get_or_insert_with(HashMap::new)
                    .insert(sql.to_string(), cumulative)
                    .unwrap_or(0);
                drop(last);
                let vm_steps = if cumulative >= previous {
                    cumulative - previous
                } else {
                    cumulative
                };
                end(
                    &sql,
                    Entry {
                        executions: 1,
                        vm_steps,
                        ..Entry::default()
                    },
                );
            }
            _ => {}
        }
    }

    if enabled() {
        connection.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT | TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(record),
        );
    }
}
