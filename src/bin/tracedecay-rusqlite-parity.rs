//! Cargo-discoverable process boundary for SQLite parity integration tests.
//!
//! This target is feature-gated so the normal TraceDecay library and CLI do not
//! import the bundled rusqlite helper. It forwards one JSON request on stdin to
//! the helper implementation and writes its one JSON response on stdout.

use std::io;

fn main() {
    if let Err(error) = tracedecay_rusqlite_parity::serve(io::stdin().lock(), io::stdout().lock()) {
        eprintln!("tracedecay-rusqlite-parity transport failure: {error}");
        std::process::exit(1);
    }
}
