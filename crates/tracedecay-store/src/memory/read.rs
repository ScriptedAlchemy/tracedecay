use std::sync::Arc;

#[derive(Clone)]
pub struct FactReadControl {
    interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl FactReadControl {
    pub fn new(interrupted: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self { interrupted }
    }

    pub fn interrupted(&self) -> bool {
        (self.interrupted)()
    }
}
