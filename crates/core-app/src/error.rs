use std::{error::Error, fmt, sync::Arc};

/// Cloneable context retains the original engine error without exposing engine types to the UI.
#[derive(Clone, Debug)]
pub struct ErrorSource {
    message: String,
    source: Option<Arc<dyn Error + Send + Sync>>,
}
impl ErrorSource {
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message: error.to_string(),
            source: Some(Arc::new(error)),
        }
    }
}
impl From<String> for ErrorSource {
    fn from(message: String) -> Self {
        Self {
            message,
            source: None,
        }
    }
}
impl From<&str> for ErrorSource {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl PartialEq for ErrorSource {
    fn eq(&self, other: &Self) -> bool {
        self.message == other.message
    }
}
impl Eq for ErrorSource {}
impl fmt::Display for ErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Error for ErrorSource {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|e| e as &(dyn Error + 'static))
    }
}

/// Categories survive transport; presentation chooses user-facing wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppError {
    SchemaTooNew { found: i64, supported: i64 },
    Corrupt(ErrorSource),
    Catalog(ErrorSource),
    NotOpen,
    InvalidCommand(String),
    Closed,
    Stale,
    Exhausted,
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaTooNew { found, supported } => write!(
                f,
                "catalog schema version {found} is newer than this build supports ({supported})"
            ),
            Self::Corrupt(detail) => detail.fmt(f),
            Self::Catalog(message) => message.fmt(f),
            Self::InvalidCommand(message) => f.write_str(message),
            Self::NotOpen => f.write_str("catalog is not open"),
            Self::Closed => f.write_str("application session is closed"),
            Self::Stale => f.write_str("obsolete application request"),
            Self::Exhausted => f.write_str("application identity counter exhausted"),
        }
    }
}
impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Corrupt(e) | Self::Catalog(e) => Some(e),
            _ => None,
        }
    }
}
