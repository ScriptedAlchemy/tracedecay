//! Decode one sealed generation file at corpus scale and report wall time,
//! resident-set peaks, and the restored corpus shape.
//!
//! Usage: `sealed_decode_profile <sealed-generation.json>`
//!
//! Build with `--features hotpath` (or `hotpath-alloc`) for span/allocation
//! attribution; the plain build reports OS-level numbers only.

use std::io::BufReader;
use std::process::ExitCode;
use std::time::Instant;

use tracedecay_code_index::production::{
    CodeIndexPublishedGenerationV1, UninterruptibleCodeIndexControlV1,
};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

fn proc_status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix(field)?
            .trim_start_matches(':')
            .trim()
            .strip_suffix(" kB")?
            .parse()
            .ok()
    })
}

fn report_memory(stage: &str) {
    let rss = proc_status_kib("VmRSS").unwrap_or(0);
    let peak = proc_status_kib("VmHWM").unwrap_or(0);
    println!(
        "{stage}: VmRSS {} MiB, VmHWM {} MiB",
        rss / 1024,
        peak / 1024
    );
}

fn main() -> ExitCode {
    #[cfg(feature = "hotpath")]
    if std::env::var_os("HOTPATH_METRICS_SERVER_OFF").is_none() {
        // The profiling run must open no socket; set before any thread exists.
        unsafe {
            std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        }
    }
    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("sealed-decode-profile").build();

    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: sealed_decode_profile <sealed-generation.json>");
        return ExitCode::FAILURE;
    };
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("sealed_decode_profile: open {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let admitted_len = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            eprintln!("sealed_decode_profile: metadata {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("sealed bytes: {admitted_len}");
    report_memory("before decode");

    let started = Instant::now();
    let decoded = CodeIndexPublishedGenerationV1::decode_sealed_seek_reader(
        BufReader::with_capacity(1024 * 1024, file),
        admitted_len,
        None,
        &UninterruptibleCodeIndexControlV1,
    );
    let elapsed = started.elapsed();

    let generation = match decoded {
        Ok(Some(generation)) => generation,
        Ok(None) => {
            eprintln!("sealed_decode_profile: incompatible format revision");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("sealed_decode_profile: decode failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("decode wall: {:.3}s", elapsed.as_secs_f64());
    println!(
        "generation {}: {} chunks, {} symbols, {} edges",
        generation.manifest().generation_id.as_str(),
        generation.chunks().chunks().len(),
        generation.symbols().symbols.len(),
        generation.edges().len(),
    );
    report_memory("after decode");

    drop(generation);
    report_memory("after drop");
    ExitCode::SUCCESS
}
