use std::io;

use tracedecay_rusqlite_parity::serve;

fn main() {
    if let Err(error) = serve(io::stdin().lock(), io::stdout().lock()) {
        eprintln!("tracedecay-rusqlite-parity transport failure: {error}");
        std::process::exit(1);
    }
}
