//! Store-free LSP diagnostics support.

pub mod activity;
pub mod adapters;
pub mod broker;
pub mod client;
pub mod settings;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("config error: {message}")]
    Config { message: String },
}

pub type Result<T> = std::result::Result<T, LspError>;
