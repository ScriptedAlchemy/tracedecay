use fixture_provenance::{git, watched_input_file::WatchedInputFile};

fn main() {
    WatchedInputFile::from_env("FIXTURE_WATCHED_INPUT").emit();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("manifest directory");
    let root = std::path::Path::new(&manifest_dir);
    let resolved = git::resolve(root)
        .expect("fixture provenance")
        .expect("fixture git worktree");
    for path in git::watch_paths(root) {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    let counter = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("output directory"))
        .join("build-script-runs");
    let runs = match std::fs::read_to_string(&counter) {
        Ok(value) => value.parse::<u32>().expect("valid run count") + 1,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
        Err(error) => panic!("read run count: {error}"),
    };
    std::fs::write(counter, runs.to_string()).expect("write run count");
    println!(
        "cargo::rustc-env=FIXTURE_IDENTITY={}",
        if resolved.dirty { "dirty" } else { "clean" }
    );
}
