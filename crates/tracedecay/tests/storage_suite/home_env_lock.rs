/// Serializes tests that mutate the process-wide HOME, USERPROFILE, or
/// profile-directory environment variables. Nextest isolates each test in its
/// own process, while plain `cargo test` shares this lock across suite modules.
pub static HOME_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
