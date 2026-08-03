pub mod claude;
pub mod cline_like;
pub mod codex;
pub mod codex_app_server;
pub mod cursor;
pub mod cursor_agent;
pub mod cursor_composer;
pub mod git_correlation;
pub mod hermes;
pub mod kiro;
pub mod lcm;
pub mod shared;
pub mod source;
pub mod transcript_backfill;
pub mod vibe;
pub mod workflow_index;
pub mod workflow_ingest;
pub mod workflow_state;

pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)
}

pub(crate) fn vscode_data_dir(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/Code")
    }
    #[cfg(target_os = "linux")]
    {
        home.join(".config/Code")
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_path = std::path::PathBuf::from(&appdata);
            if appdata_path.starts_with(home) {
                return appdata_path.join("Code");
            }
        }
        home.join("AppData/Roaming/Code")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        home.join(".config/Code")
    }
}

pub(crate) fn kiro_data_dir(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/Kiro")
    }
    #[cfg(target_os = "linux")]
    {
        home.join(".config/Kiro")
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_path = std::path::PathBuf::from(&appdata);
            if appdata_path.starts_with(home) {
                return appdata_path.join("Kiro");
            }
        }
        home.join("AppData/Roaming/Kiro")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        home.join(".config/Kiro")
    }
}
