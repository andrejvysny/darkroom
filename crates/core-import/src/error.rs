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

impl ImportError {
    /// The underlying RAW-decode failure, when this error is one (an import failure reaches us
    /// wrapped one level deeper than in `core-library`: `Lib(LibError::Raw(_))`).
    pub fn as_raw(&self) -> Option<&core_raw::RawError> {
        match self {
            ImportError::Lib(e) => e.as_raw(),
            _ => None,
        }
    }
}
