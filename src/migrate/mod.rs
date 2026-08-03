//! Compatibility façade for the extracted migration subsystem.

pub mod consolidate {
    pub use tracedecay_migrate::consolidate::*;
}

pub mod hermes {
    pub use tracedecay_migrate::hermes::*;
}

pub mod inventory {
    pub use tracedecay_migrate::inventory::*;
}

pub mod manifest {
    pub use tracedecay_migrate::manifest::*;
}

pub mod registry {
    pub use tracedecay_migrate::registry::*;
}
