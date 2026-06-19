use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum OdinError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error in {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("invalid config in {path}: {message}")]
    InvalidConfig { path: PathBuf, message: String },
    #[error("duplicate service name: {0}")]
    DuplicateService(String),
    #[error("service not found: {0}")]
    ServiceNotFound(String),
    #[error("service is already running: {0}")]
    AlreadyRunning(String),
    #[error("service is not running: {0}")]
    NotRunning(String),
    #[error("control protocol error: {0}")]
    Protocol(String),
    #[error("nix error: {0}")]
    Nix(#[from] nix::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, OdinError>;
