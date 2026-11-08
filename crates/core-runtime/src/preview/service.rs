//! The preview worker thread: pick the next photo, skip it when its tier already exists, ask the
//! host port to render it, and publish the result under the catalog's publication fence.
//!
//! Same shape as [`crate::sidecar::MirrorService`]: a runtime-owned thread with an ordered,
//! idempotent shutdown (also run on drop), so the host can stop it before the GPU or the catalog go.

use super::queue::{Next, Queue, PAGE_SIZE};
use super::{PreviewHandle, PreviewJob, PreviewRenderError, PreviewRenderPort, RenderedPreview};
use crate::catalog::{CatalogAdapter, SharedDb};
use crate::import::panic_text;
use crate::publication::{PublicationToken, SourceKey};
use core_db::rusqlite::{named_params, OptionalExtension};
use core_library::ThumbCache;
use core_model::identity::{ImageId, CURRENT_PROCESS_VERSION};
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// Cache-cap enforcement is a directory scan: run it once per this many published previews.
const EVICT_EVERY: u64 = 24;
/// Pause after a refused render so a busy host is not polled in a tight loop.
const SKIP_BACKOFF: Duration = Duration::from_millis(50);
/// How often a service started before any catalog opened looks again.
const NO_CATALOG_POLL: Duration = Duration::from_secs(1);

type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct PreviewService {
    handle: PreviewHandle,
    thread: Option<JoinHandle<()>>,
}

impl PreviewService {
    /// Start the worker. The first backfill pass over the library begins at once, behind anything
    /// prioritized. `wake` runs after every published preview (the host repaints).
    pub fn start(
        catalog: Arc<CatalogAdapter>,
        thumbs_root: PathBuf,
        port: Arc<dyn PreviewRenderPort>,
        wake: Wake,
    ) -> Result<Self, String> {
        let thumbs = ThumbCache::new(thumbs_root)
            .map_err(|error| format!("could not open the thumbnail cache: {error}"))?;
        let queue = Arc::new(Queue::new());
        let worker = Worker {
            queue: Arc::clone(&queue),
            catalog,
            thumbs,
            port,
            wake,
            writes: 0,
            dead: HashSet::new(),
            last_generation: None,
        };
        let thread = std::thread::Builder::new()
            .name("preview-service".into())
            .spawn(move || worker.run())
            .map_err(|error| format!("could not start the preview service: {error}"))?;
        Ok(Self {
            handle: PreviewHandle { queue },
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> PreviewHandle {
        self.handle.clone()
    }

    pub fn prioritize(&self, ids: &[i64]) {
        self.handle.prioritize(ids);
    }

    pub fn enqueue_back(&self, ids: &[i64]) {
        self.handle.enqueue_back(ids);
    }

    pub fn set_paused(&self, paused: bool) {
        self.handle.set_paused(paused);
    }

    /// Stop and join. Idempotent; waits for at most the render in hand, even while paused.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.handle.queue.stop();
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => Err("the preview service thread died".into()),
            _ => Ok(()),
        }
    }
}

impl Drop for PreviewService {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            tracing::error!(%error, "preview service shutdown");
        }
    }
}

/// What the catalog says about one photo right now.
struct Row {
    hash: String,
    path: PathBuf,
    edited_at: Option<i64>,
}

fn read_row(db: &SharedDb, id: i64) -> Option<Row> {
    let db = db.lock().unwrap_or_else(PoisonError::into_inner);
    db.conn
        .query_row(
            "SELECT i.content_hash, i.path, e.updated_at
               FROM images i LEFT JOIN edits e ON e.image_id = i.id
              WHERE i.id = :id AND i.status = 'present'",
            named_params! { ":id": id },
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
        .map(|(hash, path, edited_at)| Row {
            hash: hash.iter().map(|byte| format!("{byte:02x}")).collect(),
            path: PathBuf::from(path),
            edited_at,
        })
}

fn read_page(db: &SharedDb, cursor: i64) -> Vec<i64> {
    let db = db.lock().unwrap_or_else(PoisonError::into_inner);
    let page = db.conn.prepare(
        "SELECT id FROM images WHERE status = 'present' AND id > :cursor ORDER BY id LIMIT :limit",
    );
    let ids = page.and_then(|mut stmt| {
        stmt.query_map(
            named_params! { ":cursor": cursor, ":limit": PAGE_SIZE as i64 },
            |r| r.get(0),
        )?
        .collect::<Result<Vec<i64>, _>>()
    });
    ids.unwrap_or_else(|error| {
        // An unreadable page ends the pass; the next process start walks the library again.
        tracing::warn!(%error, "preview backfill page unreadable");
        Vec::new()
    })
}

struct Worker {
    queue: Arc<Queue>,
    catalog: Arc<CatalogAdapter>,
    thumbs: ThumbCache,
    port: Arc<dyn PreviewRenderPort>,
    wake: Wake,
    writes: u64,
    /// Photos the host cannot render at this edit version; not asked again this run.
    dead: HashSet<(i64, Option<i64>)>,
    last_generation: Option<u64>,
}

impl Worker {
    fn run(mut self) {
        let outcome = catch_unwind(AssertUnwindSafe(|| self.drive()));
        if let Err(payload) = outcome {
            tracing::error!(panic = %panic_text(&*payload), "preview service stopped after a panic");
        }
    }

    fn drive(&mut self) {
        loop {
            match self.queue.next() {
                Next::Stop => return,
                Next::Refill { cursor, generation } => self.refill(cursor, generation),
                Next::Render(id) => self.render(id),
            }
        }
    }

    /// The open catalog, restarting the backfill when it is not the one the queue was built for.
    fn current_catalog(&mut self) -> Option<(u64, SharedDb)> {
        let (generation, db) = self.catalog.shared().ok()?;
        if self.last_generation != Some(generation) {
            if self.last_generation.is_some() {
                self.queue.reset();
                self.dead.clear();
            }
            self.last_generation = Some(generation);
        }
        Some((generation, db))
    }

    fn refill(&mut self, cursor: i64, scan: u64) {
        let Some((_, db)) = self.current_catalog() else {
            self.queue.nap(NO_CATALOG_POLL);
            return;
        };
        let ids = read_page(&db, cursor);
        self.queue.refill(scan, &ids);
    }

    fn tier_exists(&self, row: &Row, edge: u32) -> bool {
        let pv = CURRENT_PROCESS_VERSION.0;
        match row.edited_at {
            Some(version) => self.thumbs.has_edited_preview(&row.hash, pv, version, edge),
            None => self.thumbs.has_preview(&row.hash, pv, edge),
        }
    }

    fn render(&mut self, id: i64) {
        let Some((generation, db)) = self.current_catalog() else {
            return;
        };
        let Some(row) = read_row(&db, id) else { return };
        if !row.path.exists() || self.dead.contains(&(id, row.edited_at)) {
            return;
        }
        let edge = self.catalog.preview_edge();
        if self.tier_exists(&row, edge) {
            return;
        }
        let key = SourceKey {
            hash: row.hash.clone(),
            path: row.path.display().to_string(),
        };
        let Ok(token) = self.catalog.publications().capture(ImageId(id), key) else {
            return;
        };
        let job = PreviewJob {
            image_id: id,
            hash: row.hash,
            path: row.path,
            edited_at: row.edited_at,
            edge,
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| self.port.render(&job)));
        match outcome {
            Ok(Ok(rendered)) => self.publish(generation, &job, &token, &rendered),
            Ok(Err(PreviewRenderError::Skip(_))) => {
                self.queue.requeue_back(id);
                self.queue.nap(SKIP_BACKOFF);
            }
            Ok(Err(PreviewRenderError::Unsupported(reason))) => {
                tracing::debug!(id, %reason, "preview not rendered by this build");
                self.dead.insert((id, job.edited_at));
            }
            Ok(Err(PreviewRenderError::Failed(reason))) => {
                tracing::warn!(id, %reason, "preview render failed");
                self.dead.insert((id, job.edited_at));
            }
            Err(payload) => {
                tracing::error!(id, panic = %panic_text(&*payload), "preview render panicked");
                self.dead.insert((id, job.edited_at));
            }
        }
    }

    fn publish(
        &mut self,
        generation: u64,
        job: &PreviewJob,
        token: &PublicationToken,
        rendered: &RenderedPreview,
    ) {
        let published = self
            .catalog
            .publications()
            .with_current(token, || self.write_if_current(generation, job, rendered))
            .unwrap_or(false);
        if !published {
            return;
        }
        (self.wake)();
        self.writes += 1;
        if self.writes.is_multiple_of(EVICT_EVERY) {
            self.enforce_cap();
        }
    }

    /// Runs inside the publication fence: the row must still be the one that was rendered.
    fn write_if_current(&self, generation: u64, job: &PreviewJob, r: &RenderedPreview) -> bool {
        let Ok((now, db)) = self.catalog.shared() else {
            return false;
        };
        if now != generation || !self.catalog.is_current(generation) {
            return false;
        }
        let Some(row) = read_row(&db, job.image_id) else {
            return false;
        };
        if row.hash != job.hash || row.edited_at != r.rendered_edit_version {
            return false;
        }
        let pv = CURRENT_PROCESS_VERSION.0;
        let (preview, thumb) = match r.rendered_edit_version {
            Some(v) => (
                self.thumbs
                    .write_edited_preview(&job.hash, pv, v, job.edge, &r.preview_jpeg),
                self.thumbs.write_edited(&job.hash, pv, v, &r.thumb_jpeg),
            ),
            None => (
                self.thumbs
                    .write_preview(&job.hash, pv, job.edge, &r.preview_jpeg),
                self.thumbs.write_canonical(&job.hash, pv, &r.thumb_jpeg),
            ),
        };
        if let Err(error) = &thumb {
            tracing::warn!(id = job.image_id, %error, "preview thumbnail not cached");
        }
        preview.is_ok()
    }

    fn enforce_cap(&self) {
        let Ok((_, db)) = self.catalog.shared() else {
            return;
        };
        let cap = {
            let db = db.lock().unwrap_or_else(PoisonError::into_inner);
            core_library::thumb_cache_cap(&db.conn)
        };
        if let Ok(cap) = cap {
            if let Err(error) = self.thumbs.evict_to(cap) {
                tracing::warn!(%error, "thumbnail cache eviction failed");
            }
        }
    }
}
