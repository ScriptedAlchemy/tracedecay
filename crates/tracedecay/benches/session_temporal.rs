use std::env;

use tracedecay::session_temporal_benchmark::{
    refresh_contract, run_measurement, validate_contract,
};

fn main() {
    let arguments = env::args()
        .skip(1)
        .filter(|argument| argument != "--bench")
        .collect::<Vec<_>>();
    let result = match arguments.as_slice() {
        [] => validate_contract(),
        [argument] if argument == "--validate-only" => validate_contract(),
        [argument] if argument == "--run" => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime");
            runtime.block_on(async {
                run_measurement().await.map(|value| {
                    println!("{}", serde_json::to_string_pretty(&value).unwrap());
                })
            })
        }
        [argument] if argument == "--refresh-contract" => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime");
            runtime.block_on(async {
                refresh_contract().await.map(|value| {
                    println!("{}", serde_json::to_string_pretty(&value).unwrap());
                })
            })
        }
        _ => Err(
            "usage: cargo test --bench session_temporal | cargo bench --bench session_temporal -- --run|--refresh-contract"
                .to_owned(),
        ),
    };
    if let Err(error) = result {
        eprintln!("session-temporal benchmark: {error}");
        std::process::exit(1);
    }
}
