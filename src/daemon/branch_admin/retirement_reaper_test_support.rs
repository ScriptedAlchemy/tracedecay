use std::sync::{Condvar, Mutex, PoisonError};

#[cfg_attr(not(unix), allow(dead_code))]
pub(in crate::daemon) struct RetirementReaperRegistrationBarrier {
    reached: tokio::sync::watch::Sender<bool>,
    released: Mutex<bool>,
    released_changed: Condvar,
}

impl RetirementReaperRegistrationBarrier {
    pub(in crate::daemon) fn new() -> Self {
        let (reached, _) = tokio::sync::watch::channel(false);
        Self {
            reached,
            released: Mutex::new(false),
            released_changed: Condvar::new(),
        }
    }

    pub(in crate::daemon) fn block(&self) {
        self.reached.send_replace(true);
        let mut released = self.released.lock().unwrap_or_else(PoisonError::into_inner);
        while !*released {
            released = self
                .released_changed
                .wait(released)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub(in crate::daemon) async fn wait_until_reached(&self) {
        let mut reached = self.reached.subscribe();
        while !*reached.borrow_and_update() {
            if reached.changed().await.is_err() {
                return;
            }
        }
    }

    pub(in crate::daemon) fn release(&self) {
        *self.released.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.released_changed.notify_all();
    }
}

impl Drop for RetirementReaperRegistrationBarrier {
    fn drop(&mut self) {
        self.release();
    }
}
