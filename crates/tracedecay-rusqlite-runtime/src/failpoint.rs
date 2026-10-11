//! Process-abort failpoints for the crash harness (`tests/crash`).
//!
//! A failpoint armed with [`arm`] aborts the process on its `nth` hit, before
//! the code after it runs, so the store holds exactly what a killed writer
//! leaves behind. Unarmed failpoints cost one uncontended lock per hit.

use std::sync::Mutex;

struct Armed {
    name: String,
    remaining: u64,
}

static ARMED: Mutex<Option<Armed>> = Mutex::new(None);

/// Arms `name` to abort the process on its `nth` hit (`nth` >= 1).
pub fn arm(name: &str, nth: u64) {
    assert!(nth >= 1, "failpoint {name} armed for hit 0");
    *ARMED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Armed {
        name: name.to_owned(),
        remaining: nth,
    });
}

pub(crate) fn hit(name: &str) {
    let mut armed = ARMED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(state) = armed.as_mut().filter(|state| state.name == name) else {
        return;
    };
    state.remaining -= 1;
    if state.remaining == 0 {
        eprintln!("TD_CRASH failpoint {name}");
        std::process::abort();
    }
}
