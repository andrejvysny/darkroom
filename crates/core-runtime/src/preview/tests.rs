//! Service tests over a real temp catalog and thumbnail cache with a scripted render port.
use super::*;
use crate::catalog::CatalogAdapter;
use crate::sidecar::test_support::Fix;
use core_db::rusqlite::params;
use core_library::ThumbCache;
use core_model::identity::CURRENT_PROCESS_VERSION;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PV: i64 = CURRENT_PROCESS_VERSION.0;
const EDGE: u32 = core_library::PREVIEW_EDGE_MIN;

type Respond =
    dyn Fn(&PreviewJob, usize) -> Result<RenderedPreview, PreviewRenderError> + Send + Sync;

/// Records every call; the first call can be held open until the test releases it.
struct Fake {
    calls: Mutex<Vec<i64>>,
    entered: Mutex<Option<Sender<()>>>,
    release: Mutex<Option<Receiver<()>>>,
    respond: Box<Respond>,
}

impl PreviewRenderPort for Fake {
    fn render(&self, job: &PreviewJob) -> Result<RenderedPreview, PreviewRenderError> {
        let nth = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(job.image_id);
            calls.iter().filter(|&&id| id == job.image_id).count()
        };
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        let release = self.release.lock().unwrap().take();
        if let Some(release) = release {
            let _ = release.recv();
        }
        (self.respond)(job, nth)
    }
}

fn ok(version: Option<i64>) -> Result<RenderedPreview, PreviewRenderError> {
    Ok(RenderedPreview {
        preview_jpeg: b"preview".to_vec(),
        thumb_jpeg: b"thumb".to_vec(),
        rendered_edit_version: version,
    })
}

struct Rig {
    fix: Fix,
    catalog: Arc<CatalogAdapter>,
    thumbs: ThumbCache,
    fake: Arc<Fake>,
    service: Option<PreviewService>,
    wakes: Arc<Mutex<u32>>,
}

fn hash_of(tag: u8) -> String {
    format!("{tag:02x}").repeat(32)
}

impl Rig {
    fn new(name: &str, respond: Box<Respond>) -> Self {
        let fix = Fix::new(&format!("preview-{name}"));
        let catalog = Arc::new(CatalogAdapter::from_shared(Arc::clone(&fix.db)));
        let root = fix.dir().join("thumbs");
        let thumbs = ThumbCache::new(&root).unwrap();
        let fake = Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            entered: Mutex::new(None),
            release: Mutex::new(None),
            respond,
        });
        Self {
            fix,
            catalog,
            thumbs,
            fake,
            service: None,
            wakes: Arc::default(),
        }
    }

    /// Hold the first render open; returns (entered, release) ends for the test.
    fn hold_first(&self) -> (Receiver<()>, Sender<()>) {
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        *self.fake.entered.lock().unwrap() = Some(entered_tx);
        *self.fake.release.lock().unwrap() = Some(release_rx);
        (entered_rx, release_tx)
    }

    fn start(&mut self) {
        let wakes = Arc::clone(&self.wakes);
        let port: Arc<dyn PreviewRenderPort> = self.fake.clone();
        self.service = Some(
            PreviewService::start(
                Arc::clone(&self.catalog),
                self.thumbs.root().to_path_buf(),
                port,
                Arc::new(move || *wakes.lock().unwrap() += 1),
            )
            .unwrap(),
        );
    }

    fn svc(&self) -> &PreviewService {
        self.service.as_ref().unwrap()
    }

    fn calls(&self) -> Vec<i64> {
        self.fake.calls.lock().unwrap().clone()
    }

    fn set_edit(&self, id: i64, version: i64) {
        let db = self.fix.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT INTO edits(image_id, process_version, params, updated_at)
                 VALUES (:id, 6, '{}', :v)
                 ON CONFLICT(image_id) DO UPDATE SET updated_at = :v",
                core_db::rusqlite::named_params! { ":id": id, ":v": version },
            )
            .unwrap();
    }
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle() {
    std::thread::sleep(Duration::from_millis(250));
}

#[test]
fn prioritized_photo_renders_before_the_backfill() {
    let mut rig = Rig::new("front", Box::new(|_, _| ok(None)));
    let a = rig.fix.insert_image(1, "a.cr3");
    let b = rig.fix.insert_image(2, "b.cr3");
    let c = rig.fix.insert_image(3, "c.cr3");
    let (entered, release) = rig.hold_first();
    rig.start();
    entered.recv().unwrap(); // a is rendering; b and c are not queued yet
    rig.svc().prioritize(&[c]);
    release.send(()).unwrap();
    wait_until("all tiers", || {
        [1, 2, 3]
            .iter()
            .all(|&t| rig.thumbs.has_preview(&hash_of(t), PV, EDGE))
    });
    assert_eq!(rig.calls(), vec![a, c, b]);
    assert!(*rig.wakes.lock().unwrap() >= 3);
}

#[test]
fn backfill_pages_through_the_whole_library() {
    let mut rig = Rig::new("paging", Box::new(|_, _| ok(None)));
    let total = 300_i64; // more than one 256-id page
    {
        let db = rig.fix.db.lock().unwrap();
        for n in 0..total {
            let path = rig.fix.dir().join(format!("p{n}.cr3"));
            std::fs::write(&path, b"x").unwrap();
            let mut hash = vec![0u8; 32];
            hash[..8].copy_from_slice(&n.to_be_bytes());
            db.conn
                .execute(
                    "INSERT INTO images(content_hash, file_size, path, original_filename,
                                        status, capture_date, imported_at)
                     VALUES (?1, 1, ?2, 'p', 'present', 1000, 0)",
                    params![hash, path.display().to_string()],
                )
                .unwrap();
        }
    }
    rig.start();
    wait_until("300 renders", || rig.calls().len() == total as usize);
    let mut calls = rig.calls();
    assert!(calls.windows(2).all(|w| w[0] < w[1]), "cursor order");
    calls.dedup();
    assert_eq!(calls.len(), total as usize);
}

#[test]
fn pause_parks_new_jobs_and_resume_continues() {
    let mut rig = Rig::new("pause", Box::new(|_, _| ok(None)));
    for tag in 1..=3 {
        rig.fix.insert_image(tag, &format!("{tag}.cr3"));
    }
    let (entered, release) = rig.hold_first();
    rig.start();
    entered.recv().unwrap();
    rig.svc().set_paused(true);
    release.send(()).unwrap();
    settle();
    assert_eq!(rig.calls().len(), 1, "no job starts while paused");
    rig.svc().set_paused(false);
    wait_until("resume", || rig.calls().len() == 3);
}

#[test]
fn existing_tiers_are_skipped() {
    let mut rig = Rig::new("precheck", Box::new(|_, _| ok(None)));
    rig.fix.insert_image(1, "a.cr3");
    let b = rig.fix.insert_image(2, "b.cr3");
    let c = rig.fix.insert_image(3, "c.cr3");
    rig.set_edit(b, 5);
    rig.thumbs
        .write_preview(&hash_of(1), PV, EDGE, b"old")
        .unwrap();
    rig.thumbs
        .write_edited_preview(&hash_of(2), PV, 5, EDGE, b"old")
        .unwrap();
    rig.start();
    wait_until("c", || rig.thumbs.has_preview(&hash_of(3), PV, EDGE));
    assert_eq!(rig.calls(), vec![c]);
}

#[test]
fn unedited_publish_writes_pv_and_dev_and_sweeps_embedded() {
    let mut rig = Rig::new("sweep", Box::new(|_, _| ok(None)));
    rig.fix.insert_image(1, "a.cr3");
    rig.thumbs
        .write_embedded_preview(&hash_of(1), EDGE, b"emb")
        .unwrap();
    rig.start();
    wait_until("pv", || rig.thumbs.has_preview(&hash_of(1), PV, EDGE));
    assert!(!rig.thumbs.has_embedded_preview(&hash_of(1), EDGE));
    assert!(rig.thumbs.has_canonical(&hash_of(1), PV));
    assert_eq!(
        rig.thumbs.read_preview(&hash_of(1), PV, EDGE).unwrap(),
        b"preview"
    );
}

#[test]
fn edited_publish_writes_editpv_and_edit() {
    let mut rig = Rig::new("edited", Box::new(|_, _| ok(Some(7))));
    let a = rig.fix.insert_image(1, "a.cr3");
    rig.set_edit(a, 7);
    rig.start();
    wait_until("editpv", || {
        rig.thumbs.has_edited_preview(&hash_of(1), PV, 7, EDGE)
    });
    assert_eq!(
        rig.thumbs.read_edited(&hash_of(1), PV, 7).unwrap(),
        b"thumb"
    );
    assert!(!rig.thumbs.has_preview(&hash_of(1), PV, EDGE));
}

#[test]
fn publication_drops_when_the_edit_changed_during_the_render() {
    let fix_db: Arc<Mutex<Option<Arc<Mutex<core_db::Db>>>>> = Arc::default();
    let seen = Arc::clone(&fix_db);
    let mut rig = Rig::new(
        "drop-edit",
        Box::new(move |job, _| {
            if job.image_id == 1 {
                // The user saved an edit while the host rendered default parameters.
                let db = seen.lock().unwrap().clone().unwrap();
                db.lock()
                    .unwrap()
                    .conn
                    .execute(
                        "INSERT INTO edits(image_id, process_version, params, updated_at)
                         VALUES (1, 6, '{}', 9)",
                        [],
                    )
                    .unwrap();
            }
            ok(None)
        }),
    );
    *fix_db.lock().unwrap() = Some(Arc::clone(&rig.fix.db));
    rig.fix.insert_image(1, "a.cr3");
    rig.fix.insert_image(2, "b.cr3");
    rig.start();
    wait_until("b", || rig.thumbs.has_preview(&hash_of(2), PV, EDGE));
    assert!(!rig.thumbs.has_preview(&hash_of(1), PV, EDGE));
    assert!(!rig.thumbs.has_edited_preview(&hash_of(1), PV, 9, EDGE));
}

#[test]
fn publication_drops_when_the_content_hash_changed_during_the_render() {
    let handle: Arc<Mutex<Option<Arc<Mutex<core_db::Db>>>>> = Arc::default();
    let seen = Arc::clone(&handle);
    let mut rig = Rig::new(
        "drop-hash",
        Box::new(move |job, _| {
            if job.image_id == 1 {
                let db = seen.lock().unwrap().clone().unwrap();
                db.lock()
                    .unwrap()
                    .conn
                    .execute(
                        "UPDATE images SET content_hash = ?1 WHERE id = 1",
                        params![vec![0x77u8; 32]],
                    )
                    .unwrap();
            }
            ok(None)
        }),
    );
    *handle.lock().unwrap() = Some(Arc::clone(&rig.fix.db));
    rig.fix.insert_image(1, "a.cr3");
    rig.fix.insert_image(2, "b.cr3");
    rig.start();
    wait_until("b", || rig.thumbs.has_preview(&hash_of(2), PV, EDGE));
    assert!(!rig.thumbs.has_preview(&hash_of(1), PV, EDGE));
    assert!(!rig.thumbs.has_preview(&hash_of(0x77), PV, EDGE));
}

#[test]
fn skip_requeues_and_unsupported_is_not_retried() {
    let mut rig = Rig::new(
        "skip",
        Box::new(|job, nth| match (job.image_id, nth) {
            (1, 1) => Err(PreviewRenderError::Skip("busy".into())),
            (2, _) => Err(PreviewRenderError::Unsupported("ai".into())),
            _ => ok(None),
        }),
    );
    rig.fix.insert_image(1, "a.cr3");
    rig.fix.insert_image(2, "b.cr3");
    rig.start();
    wait_until("a", || rig.thumbs.has_preview(&hash_of(1), PV, EDGE));
    rig.svc().prioritize(&[2]); // asking again must not re-render a known-unsupported edit
    settle();
    let calls = rig.calls();
    assert_eq!(calls.iter().filter(|&&id| id == 1).count(), 2);
    assert_eq!(calls.iter().filter(|&&id| id == 2).count(), 1);
}

#[test]
fn shutdown_joins_while_paused_with_work_queued() {
    let mut rig = Rig::new("shutdown", Box::new(|_, _| ok(None)));
    for tag in 1..=5 {
        rig.fix.insert_image(tag, &format!("{tag}.cr3"));
    }
    let (entered, release) = rig.hold_first();
    rig.start();
    entered.recv().unwrap();
    rig.svc().set_paused(true);
    release.send(()).unwrap();
    settle();
    let started = Instant::now();
    let service = rig.service.as_mut().unwrap();
    service.shutdown().unwrap();
    service.shutdown().unwrap(); // idempotent
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(rig.calls().len(), 1);
}
