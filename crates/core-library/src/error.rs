use thiserror::Error;

#[derive(Debug, Error)]
pub enum LibError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(#[from] core_db::DbError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] core_db::rusqlite::Error),
    #[error("raw: {0}")]
    Raw(#[from] core_raw::RawError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

impl LibError {
    /// The underlying RAW-decode failure, when this error is one. Lets the catalog classify a
    /// per-file indexing failure (`unsupported` vs `corrupt` vs `io`) without matching on strings.
    pub fn as_raw(&self) -> Option<&core_raw::RawError> {
        match self {
            LibError::Raw(e) => Some(e),
            _ => None,
        }
    }
}
