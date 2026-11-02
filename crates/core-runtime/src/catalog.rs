use crate::publication::PublicationGate;
use core_app::workflow::{ColorLabel, CullChange, Mutation, MutationReport};
use core_app::{
    error::ErrorSource,
    ports::{CatalogPort, SaveReceipt},
    state::{ImageTags, MirrorStatus},
    AppError,
};
use core_db::{rusqlite, Db, DbError};
use core_library::{LibError, MirrorState};
use core_model::catalog::{
    CollectionRow, DateTreeYear, FolderRow, ImageRow, KeywordRow, QueryParams, RejectSummary,
};
use core_model::identity::{CatalogRevision, ImageId};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub type SharedDb = Arc<Mutex<Db>>;

/// Moves one file to the OS Trash; `true` when the file is gone.
pub type TrashRemover = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

#[derive(Default)]
struct Slot {
    generation: u64,
    database: Option<SharedDb>,
}

#[derive(Default)]
pub struct CatalogAdapter {
    slot: Mutex<Slot>,
    /// The fence every write this adapter makes takes BEFORE the catalog mutex, and the same one
    /// the host's sidecar drain fences its publication against. It is per adapter, not per slot:
    /// a closed catalog has no identities to fence, and a re-opened one starts empty anyway.
    publications: Arc<PublicationGate>,
    /// Test seam: replaces the OS Trash for `Mutation::DeleteRejected`.
    trash: Option<TrashRemover>,
}

/// Open the catalog for any host — the legacy one that shares the connection, and `CatalogPort::open`
/// alike. Both therefore back up before migrating: an existing catalog below the current schema is
/// snapshotted into a `backups` directory next to it before `MIGRATIONS` rewrite it
/// ([`Db::open_with_upgrade_backup`], whose docs make the snapshot a pre-condition: migrations are
/// irreversible, so without it one bad migration on a user's only catalog is data loss). The
/// pre-upgrade copies are excluded from rotation, so the daily backup never rotates them away.
pub fn open_database(path: &Path) -> Result<SharedDb, AppError> {
    // `path.parent()` is only `None` for a bare filename; anchoring the backups next to the
    // catalog is what the corruption banner ("restore from the backups folder next to the
    // catalog") promises, so fall back to the current directory rather than skipping the backup.
    let backups_dir = path.parent().unwrap_or(Path::new(".")).join("backups");
    Db::open_with_upgrade_backup(path, &backups_dir)
        .map(|(db, _snapshot)| Arc::new(Mutex::new(db)))
        .map_err(db_error)
}

impl CatalogAdapter {
    pub fn from_shared(database: SharedDb) -> Self {
        Self {
            slot: Mutex::new(Slot {
                generation: 0,
                database: Some(database),
            }),
            publications: Arc::new(PublicationGate::default()),
            trash: None,
        }
    }

    /// The fence the host's sidecar drain (see [`crate::sidecar::MirrorService`]) publishes under:
    /// every catalog write this adapter makes holds it across its transaction, so a drain can never
    /// land a sidecar that contradicts a commit in flight.
    pub fn publications(&self) -> Arc<PublicationGate> {
        Arc::clone(&self.publications)
    }

    /// Route delete-rejected file removal through `remove` instead of the OS Trash.
    pub fn with_trash(mut self, remove: TrashRemover) -> Self {
        self.trash = Some(remove);
        self
    }

    /// The open connection and its generation, for long-running services (import) that lock it
    /// in short chunks themselves. Hold the `Arc` only for the run; check
    /// [`Self::is_current`] between chunks and stop when the catalog was closed or replaced.
    pub fn shared(&self) -> Result<(u64, SharedDb), AppError> {
        let slot = self
            .slot
            .lock()
            .map_err(|e| AppError::Catalog(e.to_string().into()))?;
        Ok((
            slot.generation,
            slot.database.clone().ok_or(AppError::NotOpen)?,
        ))
    }

    /// Whether `generation` (from [`Self::shared`]) still names the open catalog.
    pub fn is_current(&self, generation: u64) -> bool {
        self.slot
            .lock()
            .map(|slot| slot.generation == generation && slot.database.is_some())
            .unwrap_or(false)
    }

    fn reserve(&self) -> Result<u64, AppError> {
        // Gate before slot, and held across the swap: a drain that captured an identity from the
        // outgoing catalog must already be fenced out when the slot changes.
        let _publications = self.publications.invalidate_all_guard();
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
        let _publications = self.publications.invalidate_all_guard();
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
        self.write(|db| query(db))
    }

    /// Like [`Self::read`] with exclusive access to the connection (batch transactions).
    fn write<T>(&self, query: impl FnOnce(&mut Db) -> Result<T, LibError>) -> Result<T, AppError> {
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
            let mut db = database
                .lock()
                .map_err(|e| AppError::Catalog(e.to_string().into()))?;
            query(&mut db).map_err(library_error)
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
    fn edit(&self, image_id: i64) -> Result<Option<(String, i64)>, AppError> {
        self.read(|db| core_library::get_edit_with_process_version(&db.conn, image_id))
    }
    fn set_cull(&self, image_id: i64, change: CullChange) -> Result<(), AppError> {
        // Fenced, but no `sidecar_outbox` intent: the rating/flag/label half of a sidecar rides in
        // the row the next edit save enqueues, so a drain must not publish across this commit.
        let _publications = self.publications.mutation_guard();
        self.read(|db| match change {
            CullChange::Rating(stars) => {
                core_library::set_rating(&db.conn, image_id, i64::from(stars))
            }
            CullChange::Flag(flag) => core_library::set_flag(&db.conn, image_id, flag.as_str()),
            CullChange::Label(label) => {
                core_library::set_label(&db.conn, image_id, label.map(ColorLabel::as_str))
            }
        })
    }
    fn keywords(&self) -> Result<Vec<KeywordRow>, AppError> {
        self.read(|db| core_library::list_keywords(&db.conn))
    }
    fn collections(&self) -> Result<Vec<CollectionRow>, AppError> {
        self.read(|db| core_library::list_collections(&db.conn))
    }
    fn mirror_status(&self, image_id: i64) -> Result<MirrorStatus, AppError> {
        self.read(|db| mirror_status_of(&db.conn, image_id))
    }
    fn image_tags(&self, image_id: i64) -> Result<ImageTags, AppError> {
        self.read(|db| {
            Ok(ImageTags {
                image: ImageId(image_id),
                keywords: core_library::keywords_for_image(&db.conn, image_id)?,
                collections: core_library::collections_for_image(&db.conn, image_id)?,
                pair: core_library::pair_info(&db.conn, image_id)?,
            })
        })
    }
    fn reject_summary(&self, params: &QueryParams) -> Result<RejectSummary, AppError> {
        self.read(|db| core_library::summarize_rejected(&db.conn, params))
    }
    fn mutate(&self, mutation: &Mutation) -> Result<MutationReport, AppError> {
        match mutation {
            // Takes the fence twice, around the plan and the finalize: the Trash step between
            // them holds neither the gate nor the catalog lock.
            Mutation::DeleteRejected(params) => self.delete_rejected(params),
            Mutation::CreateCollection {
                name,
                smart,
                images,
            } => {
                let _publications = self.publications.mutation_guard();
                self.write(|db| create_collection(db, name, smart.as_deref(), images))
                    .map(MutationReport::CollectionCreated)
            }
            Mutation::RemoveFromLibrary { images } => {
                let _publications = self.publications.mutation_guard();
                self.write(|db| core_library::remove_from_catalog(&db.conn, &ids(images)))
                    .map(MutationReport::RemovedFromLibrary)
            }
            other => {
                let _publications = self.publications.mutation_guard();
                self.write(|db| apply(db, other))
                    .map(|()| MutationReport::Done)
            }
        }
    }
    fn save_edit(
        &self,
        image_id: i64,
        content_hash: &str,
        process_version: i64,
        params_json: &str,
    ) -> Result<SaveReceipt, AppError> {
        // The commit that enqueues the mirror intent, so it is fenced: the drain either sees the
        // new revision or not at all, never a half-committed edit.
        let _publications = self.publications.mutation_guard();
        self.read(|db| {
            let revision = match core_library::edits::save_edit_expecting_source(
                &db.conn,
                image_id,
                content_hash,
                process_version,
                params_json,
                core_library::now_epoch(),
            ) {
                Ok(revision) => revision,
                Err(core_library::edits::SaveError::SourceChanged) => {
                    return Ok(Err(AppError::SourceChanged));
                }
                Err(core_library::edits::SaveError::Lib(error)) => return Err(error),
            };
            let mirror = mirror_status_of(&db.conn, image_id)?;
            Ok(Ok(SaveReceipt {
                catalog_revision: CatalogRevision(revision),
                mirror,
            }))
        })?
    }
}

/// The sidecar mirror state of one image's saved edit, as the application layer sees it.
pub(crate) fn mirror_status_of(
    conn: &rusqlite::Connection,
    image_id: i64,
) -> Result<MirrorStatus, LibError> {
    Ok(match core_library::mirror_state(conn, image_id)? {
        Some(state) if state.mirrored_revision >= state.revision => MirrorStatus::Mirrored {
            revision: CatalogRevision(state.revision),
        },
        Some(MirrorState {
            last_error: Some(error),
            ..
        }) => MirrorStatus::Failing { error },
        Some(state) => MirrorStatus::Pending {
            owed_revision: CatalogRevision(state.revision),
        },
        None => MirrorStatus::Unknown,
    })
}

impl CatalogAdapter {
    /// Plan under the lock, trash with NO lock held, then finalize under the lock — unless the
    /// catalog was closed or replaced meanwhile, in which case the new catalog is left untouched
    /// (files already in the Trash stay recoverable). Thumbnail GC is not done here.
    ///
    /// The publication fence is taken with each lock, never across the Trash call: holding a gate
    /// over filesystem work would let a slow Trash operation block every sidecar publication.
    fn delete_rejected(&self, params: &QueryParams) -> Result<MutationReport, AppError> {
        let (generation, database) = self.shared()?;
        let plan = {
            let _publications = self.publications.mutation_guard();
            let db = database
                .lock()
                .map_err(|e| AppError::Catalog(e.to_string().into()))?;
            core_library::plan_delete_rejected(&db.conn, params).map_err(library_error)?
        };
        let (gone, failed) = match &self.trash {
            Some(remove) => core_library::trash::trash_files(&plan.targets, remove.as_ref()),
            None => {
                let ctx = core_library::trash::make_trash_ctx();
                core_library::trash::trash_files(&plan.targets, &move |p| ctx.delete(p).is_ok())
            }
        };
        if !self.is_current(generation) {
            return Err(AppError::Stale);
        }
        let result = {
            let _publications = self.publications.mutation_guard();
            let db = database
                .lock()
                .map_err(|e| AppError::Catalog(e.to_string().into()))?;
            core_library::finalize_delete_rejected(&db.conn, &plan, &gone, failed)
                .map_err(library_error)?
        };
        Ok(MutationReport::RejectedDeleted(result))
    }
}

fn ids(images: &[ImageId]) -> Vec<i64> {
    images.iter().map(|id| id.0).collect()
}

fn create_collection(
    db: &mut Db,
    name: &str,
    smart: Option<&QueryParams>,
    images: &[ImageId],
) -> Result<i64, LibError> {
    let tx = db.conn.transaction()?;
    let id = match smart {
        Some(query) => {
            let json = serde_json::to_string(query).map_err(|e| LibError::Other(e.to_string()))?;
            core_library::create_collection(&tx, name, true, Some(&json))?
        }
        None => {
            let id = core_library::create_collection(&tx, name, false, None)?;
            core_library::add_images_to_collection(&tx, id, &ids(images))?;
            id
        }
    };
    tx.commit()?;
    Ok(id)
}

fn apply(db: &mut Db, mutation: &Mutation) -> Result<(), LibError> {
    match mutation {
        // Catalog-only, like the single `set_cull`: no `sidecar_outbox` intent is queued for a
        // cull mark, so the next edit save is what carries rating/flag/label to the sidecar.
        Mutation::Cull { images, change } => {
            let images = ids(images);
            match change {
                CullChange::Rating(stars) => {
                    core_library::set_rating_many(&mut db.conn, &images, i64::from(*stars))
                }
                CullChange::Flag(flag) => {
                    core_library::set_flag_many(&mut db.conn, &images, flag.as_str())
                }
                CullChange::Label(label) => core_library::set_label_many(
                    &mut db.conn,
                    &images,
                    label.map(ColorLabel::as_str),
                ),
            }
        }
        Mutation::AddKeyword { images, name } => {
            let tx = db.conn.transaction()?;
            core_library::add_keyword_to_images(&tx, &ids(images), name.trim())?;
            tx.commit()?;
            Ok(())
        }
        Mutation::RemoveKeyword { images, keyword_id } => {
            let tx = db.conn.transaction()?;
            for image in images {
                core_library::remove_keyword_from_image(&tx, image.0, *keyword_id)?;
            }
            tx.commit()?;
            Ok(())
        }
        Mutation::DeleteKeyword(id) => core_library::delete_keyword(&db.conn, *id),
        Mutation::RenameCollection { id, name } => {
            core_library::rename_collection(&db.conn, *id, name)
        }
        Mutation::DeleteCollection(id) => core_library::delete_collection(&db.conn, *id),
        Mutation::AddToCollection { id, images } => {
            core_library::add_images_to_collection(&db.conn, *id, &ids(images)).map(|_| ())
        }
        Mutation::RemoveFromCollection { id, images } => {
            core_library::remove_images_from_collection(&db.conn, *id, &ids(images)).map(|_| ())
        }
        Mutation::UnlinkPair { secondary } => core_library::unlink_pair(&db.conn, secondary.0),
        Mutation::CreateCollection { .. }
        | Mutation::DeleteRejected(_)
        | Mutation::RemoveFromLibrary { .. } => Err(LibError::Other(
            "mutation is handled before the generic path".into(),
        )),
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
