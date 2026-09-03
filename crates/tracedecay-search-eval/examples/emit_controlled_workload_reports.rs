//! Emit machine-readable framed-log and cursor-parse reports.
//!
//! Used by the search-eval harness to compare capture/private-fs feature-off
//! vs feature-on durable results. This example does not add production
//! annotations.

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: emit_controlled_workload_reports <report-dir>");
        std::process::exit(2);
    };
    if let Err(error) =
        tracedecay_search_eval::write_controlled_workload_reports(std::path::Path::new(&dir))
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
