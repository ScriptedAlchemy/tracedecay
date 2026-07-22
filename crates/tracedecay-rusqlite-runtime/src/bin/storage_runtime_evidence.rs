use std::{env, path::Path};

use tracedecay_rusqlite_runtime::evidence::{EvidenceCommand, execute, run_crash_worker};

fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments.first().map(String::as_str) == Some("--storage-runtime-crash-worker") {
        let result = match arguments.as_slice() {
            [_, database, ready] => run_crash_worker(Path::new(database), Path::new(ready)),
            _ => {
                eprintln!("storage-runtime-evidence: invalid crash worker invocation");
                std::process::exit(2);
            }
        };
        if let Err(error) = result {
            eprintln!("storage-runtime-evidence: {error}");
            std::process::exit(2);
        }
        return;
    }

    let result = EvidenceCommand::parse(arguments).and_then(execute);
    match result {
        Ok(document) => println!("{document}"),
        Err(error) => {
            eprintln!("storage-runtime-evidence refused: {error}");
            std::process::exit(2);
        }
    }
}
