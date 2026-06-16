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
}
