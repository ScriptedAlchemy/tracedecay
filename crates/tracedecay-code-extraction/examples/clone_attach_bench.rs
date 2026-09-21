//! Frozen hillclimb harness for first-run source extraction.
//!
//! Walks the Rust sources of a checkout, runs the production extractor over
//! each, and reports the wall time plus a digest of every clone body. The
//! digest is the output gate: any change that moves it changed persisted
//! records.
//!
//! cargo run --release --example clone_attach_bench -- <root> [runs]

use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};
use tracedecay_code_extraction::{
    CloneBodyEligibilityV1, ConservativeCloneTokenV1, LanguageExtractor, RustExtractor,
};

fn rust_sources(root: &Path) -> Vec<(String, String)> {
    let mut stack = vec![root.to_path_buf()];
    let mut files: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !matches!(name.as_ref(), "target" | ".git" | "vendor" | "fixtures") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path).ok()?;
            let logical = path.strip_prefix(root).unwrap_or(&path);
            Some((logical.to_string_lossy().into_owned(), source))
        })
        .collect()
}

struct Census {
    bodies: u64,
    eligible: u64,
    too_small: u64,
    too_large: u64,
    incomplete: u64,
    conservative_tokens: u64,
    rename_tokens: u64,
    rename_replaced: u64,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(
        args.next()
            .expect("usage: clone_attach_bench <root> [runs]"),
    );
    let runs: usize = args.next().map_or(3, |value| value.parse().expect("runs"));

    let sources = rust_sources(&root);
    eprintln!("files={}", sources.len());

    let mut elapsed_ms = Vec::with_capacity(runs);
    let mut digest_seen: Option<String> = None;
    let mut census = None;

    for run in 0..runs {
        let mut hasher = Sha256::new();
        let mut counts = Census {
            bodies: 0,
            eligible: 0,
            too_small: 0,
            too_large: 0,
            incomplete: 0,
            conservative_tokens: 0,
            rename_tokens: 0,
            rename_replaced: 0,
        };
        let started = Instant::now();
        for (path, source) in &sources {
            let artifact = RustExtractor.extract_artifact(path, source);
            for body in &artifact.clone_bodies {
                counts.bodies += 1;
                match body.eligibility {
                    CloneBodyEligibilityV1::Eligible => counts.eligible += 1,
                    CloneBodyEligibilityV1::ExcludedTooSmall { .. } => counts.too_small += 1,
                    CloneBodyEligibilityV1::ExcludedTooLarge { .. } => counts.too_large += 1,
                    CloneBodyEligibilityV1::ExcludedIncompleteTokenization => {
                        counts.incomplete += 1
                    }
                }
                counts.conservative_tokens += body.conservative_tokens.len() as u64;
                if let Some(tokens) = body.rename_tokens.as_deref() {
                    counts.rename_tokens += tokens.len() as u64;
                    counts.rename_replaced +=
                        std::iter::zip(body.conservative_tokens.iter(), tokens.iter())
                            .filter(|(left, right)| match (left, right) {
                                (
                                    ConservativeCloneTokenV1::Syntax { text: left, .. },
                                    ConservativeCloneTokenV1::Syntax { text: right, .. },
                                ) => left != right,
                                _ => false,
                            })
                            .count() as u64;
                }
                hasher.update(
                    serde_json::to_vec(body)
                        .expect("clone body serializes")
                        .as_slice(),
                );
            }
        }
        elapsed_ms.push(started.elapsed().as_millis() as u64);
        let digest = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        match &digest_seen {
            None => digest_seen = Some(digest),
            Some(prior) => assert_eq!(prior, &digest, "clone-body digest is not deterministic"),
        }
        census = Some(counts);
        eprintln!("run {} = {} ms", run, elapsed_ms[run]);
    }

    elapsed_ms.sort_unstable();
    let census = census.expect("one run");
    println!("median_ms={}", elapsed_ms[elapsed_ms.len() / 2]);
    println!("runs_ms={elapsed_ms:?}");
    println!("clone_body_digest={}", digest_seen.expect("digest"));
    println!(
        "bodies={} eligible={} too_small={} too_large={} incomplete={}",
        census.bodies, census.eligible, census.too_small, census.too_large, census.incomplete
    );
    println!(
        "conservative_tokens={} rename_tokens={} rename_replaced_tokens={}",
        census.conservative_tokens, census.rename_tokens, census.rename_replaced
    );
}
