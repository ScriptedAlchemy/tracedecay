mod badge;

pub struct Widget {
    pub label: String,
}

impl Widget {
    pub fn label(&self) -> &str {
        &self.label
    }
}

pub trait Show {
    fn show(&self) -> &str;
}

impl Show for Widget {
    fn show(&self) -> &str {
        &self.label
    }
}

impl std::fmt::Display for Widget {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

pub struct Counter;

impl Show for Counter {
    fn show(&self) -> &str {
        "0"
    }
}
