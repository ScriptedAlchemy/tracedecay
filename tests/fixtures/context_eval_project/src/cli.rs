//! Command-line dispatch: parses the first argument as a subcommand name
//! and routes it to the auth or storage modules.

use crate::auth::login::authenticate;
use crate::storage::config_store::ConfigStore;

/// Parses `args` and dispatches to the matching subcommand handler.
pub fn run(args: &[String]) {
    match args.get(1).map(std::string::String::as_str) {
        Some("login") => run_login(args),
        Some("show-config") => run_show_config(),
        _ => print_usage(),
    }
}

fn run_login(args: &[String]) {
    let username = args.get(2).cloned().unwrap_or_default();
    let password = args.get(3).cloned().unwrap_or_default();
    match authenticate(&username, &password) {
        Ok(session) => println!("logged in, token={}", session.token),
        Err(err) => eprintln!("login failed: {err}"),
    }
}

fn run_show_config() {
    let store = ConfigStore::new("config.toml");
    match store.read_config() {
        Ok(contents) => println!("{contents}"),
        Err(err) => eprintln!("could not read config: {err}"),
    }
}

fn print_usage() {
    println!("usage: cli <login|show-config> [args...]");
}
