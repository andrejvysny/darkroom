use thiserror::Error;

#[derive(Debug, Error)]
pub enum DedupError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] core_db::rusqlite::Error),
    #[error("trash: {0}")]
    Trash(#[from] trash::Error),
}
