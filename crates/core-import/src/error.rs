use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] core_db::rusqlite::Error),
    #[error("library: {0}")]
    Lib(#[from] core_library::LibError),
    #[error("trash: {0}")]
    Trash(#[from] trash::Error),
}
