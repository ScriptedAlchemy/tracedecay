/// Formats a ledger greeting. The body deliberately contains a comma-joined
/// separator, both quote characters, and a `$` so edit-tool eval scenarios
/// must survive the shell-quoting gauntlet.
fn format_greeting(first: &str, last: &str) -> String {
    let sep = ", ";
    let cost = "$12.50";
    format!("Hello '{first}'{sep}\"{last}\" — total {cost}")
}

fn alpha() -> i32 {
    41
}

fn beta() -> i32 {
    alpha() + 1
}

fn main() {
    println!("{} {}", format_greeting("Ada", "Lovelace"), beta());
}
