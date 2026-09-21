use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("failed to determine the current directory: {0}")]
    CurrentDir(#[source] std::io::Error),
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("state database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("git operation failed: {0}")]
    Git(String),
    #[error("search error: {0}")]
    Search(String),
    #[error("workspace is already being modified")]
    WorkspaceLocked,
    #[error("task failed: {0}")]
    Task(String),
}

pub type Result<T> = std::result::Result<T, Error>;
