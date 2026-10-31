use core_app::{error::ErrorSource, ports::CatalogPort, AppError};
use core_db::{rusqlite, Db, DbError};
use core_library::LibError;
use core_model::catalog::{DateTreeYear, FolderRow, ImageRow, QueryParams};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

type SharedDb = Arc<Mutex<Db>>;

#[derive(Default)]
struct Slot {
    generation: u64,
    database: Option<SharedDb>,
}

#[derive(Default)]
pub struct CatalogAdapter {
    slot: Mutex<Slot>,
}

/// Explicit bridge for the legacy host, which still shares its catalog connection.
pub fn open_database(path: &Path) -> Result<SharedDb, AppError> {
    Db::open(path)
        .map(|db| Arc::new(Mutex::new(db)))
        .map_err(db_error)
}

impl CatalogAdapter {
    pub fn from_shared(database: SharedDb) -> Self {
        Self {
            slot: Mutex::new(Slot {
                generation: 0,
                database: Some(database),
            }),
        }
    }

    fn reserve(&self) -> Result<u64, AppError> {
        let (generation, old) = {
            let mut slot = self
                .slot
                .lock()
                .map_err(|e| AppError::Catalog(e.to_string().into()))?;
            slot.generation = slot.generation.checked_add(1).ok_or(AppError::Exhausted)?;
            (slot.generation, slot.database.take())
        };
        drop(old);
        Ok(generation)
    }

    fn open_with(&self, open: impl FnOnce() -> Result<SharedDb, AppError>) -> Result<(), AppError> {
        let generation = self.reserve()?;
        let result = open();
        let mut slot = self
            .slot
            .lock()
            .map_err(|e| AppError::Catalog(e.to_string().into()))?;
        if slot.generation != generation {
            drop(slot);
            drop(result);
            return Err(AppError::Stale);
        }
        slot.database = Some(result?);
        Ok(())
    }

    fn read<T>(&self, query: impl FnOnce(&Db) -> Result<T, LibError>) -> Result<T, AppError> {
        let (generation, database) = {
            let slot = self
                .slot
                .lock()
                .map_err(|e| AppError::Catalog(e.to_string().into()))?;
            (
                slot.generation,
                slot.database.clone().ok_or(AppError::NotOpen)?,
            )
        };
        let result = {
            let db = database
                .lock()
                .map_err(|e| AppError::Catalog(e.to_string().into()))?;
            query(&db).map_err(library_error)
        };
        let slot = self
            .slot
            .lock()
            .map_err(|e| AppError::Catalog(e.to_string().into()))?;
        if slot.generation != generation {
            return Err(AppError::Stale);
        }
        result
    }
}

impl CatalogPort for CatalogAdapter {
    fn open(&self, path: &Path) -> Result<(), AppError> {
        self.open_with(|| open_database(path))
    }

    fn close(&self) -> Result<(), AppError> {
        self.reserve().map(|_| ())
    }
    fn query(&self, params: &QueryParams) -> Result<Vec<ImageRow>, AppError> {
        self.read(|db| core_library::query_images(&db.conn, params))
    }
    fn count(&self, params: &QueryParams) -> Result<i64, AppError> {
        self.read(|db| core_library::count_images(&db.conn, params))
    }
    fn folders(&self) -> Result<Vec<FolderRow>, AppError> {
        self.read(|db| core_library::list_folders(&db.conn))
    }
    fn dates(&self) -> Result<Vec<DateTreeYear>, AppError> {
        self.read(|db| core_library::date_tree(&db.conn))
    }
    fn image(&self, id: i64) -> Result<Option<ImageRow>, AppError> {
        self.read(|db| core_library::image_by_id(&db.conn, id))
    }
}

fn is_corrupt(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code,
        rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase))
}

fn db_error(error: DbError) -> AppError {
    match error {
        DbError::SchemaTooNew { found, supported } => AppError::SchemaTooNew { found, supported },
        DbError::Corrupt(_) => AppError::Corrupt(ErrorSource::new(error)),
        DbError::Sqlite(ref source) if is_corrupt(source) => {
            AppError::Corrupt(ErrorSource::new(error))
        }
        _ => AppError::Catalog(ErrorSource::new(error)),
    }
}

fn library_error(error: LibError) -> AppError {
    match error {
        LibError::Db(source) => db_error(source),
        LibError::Sqlite(ref source) if is_corrupt(source) => {
            AppError::Corrupt(ErrorSource::new(error))
        }
        _ => AppError::Catalog(ErrorSource::new(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    fn memory_db() -> SharedDb {
        Arc::new(Mutex::new(Db::open_in_memory().unwrap()))
    }

    #[test]
    fn close_invalidates_read_in_progress() {
        let adapter = Arc::new(CatalogAdapter::from_shared(memory_db()));
        let (started, observed) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let reader = Arc::clone(&adapter);
        let task = thread::spawn(move || {
            reader.read(|_| {
                started.send(()).unwrap();
                proceed.recv().unwrap();
                Ok(42)
            })
        });
        observed.recv().unwrap();
        adapter.close().unwrap();
        release.send(()).unwrap();
        assert_eq!(task.join().unwrap(), Err(AppError::Stale));
        assert_eq!(
            adapter.count(&QueryParams::default()),
            Err(AppError::NotOpen)
        );
    }

    #[test]
    fn replacement_invalidates_open_in_progress() {
        let adapter = Arc::new(CatalogAdapter::default());
        let (started, observed) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let opener = Arc::clone(&adapter);
        let task = thread::spawn(move || {
            opener.open_with(|| {
                started.send(()).unwrap();
                proceed.recv().unwrap();
                Ok(memory_db())
            })
        });
        observed.recv().unwrap();
        adapter.close().unwrap();
        adapter.open_with(|| Ok(memory_db())).unwrap();
        release.send(()).unwrap();
        assert_eq!(task.join().unwrap(), Err(AppError::Stale));
        assert_eq!(adapter.count(&QueryParams::default()).unwrap(), 0);
    }
}
