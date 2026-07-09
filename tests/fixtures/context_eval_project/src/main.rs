//! Toy CLI entry point for the tracedecay_context relevance eval fixture.
//!
//! This crate is a deliberately small, hand-built project (not a copy of
//! any real code) used as a frozen corpus for scoring `tracedecay_context`
//! query relevance. Do not "clean up" or expand it casually — the labeled
//! queries in `tests/fixtures/context_eval_labeled.json` assert against the
//! exact qualified names of the symbols defined here.

mod auth;
mod cli;
mod net;
mod storage;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    cli::run(&args);
}
