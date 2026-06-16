use thiserror::Error;

#[derive(Debug, Error)]
pub enum DedupError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] core_db::rusqlite::Error),
    #[error("trash: {0}")]
    Trash(#[from] trash::Error),
    /// The keeper passed to `resolve` is not a present catalog row — refuse to trash anything
    /// rather than risk deleting the last copy of a group.
    #[error("invalid keeper: {0}")]
    InvalidKeeper(String),
}
