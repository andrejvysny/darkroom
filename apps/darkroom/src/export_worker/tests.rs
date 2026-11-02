use super::*;
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn job(count: i64) -> ExportJob {
    ExportJob {
        items: (1..=count)
            .map(|id| ExportItem {
                image: ImageId(id),
                file_name: format!("{id}.jpg"),
            })
            .collect(),
        dir: PathBuf::from("/out"),
        format: ExportFormat::Jpeg,
        quality: 92,
        open_doc: None,
    }
}

fn finished(worker: &ExportWorker) -> (Vec<ExportEvent>, ExportEvent) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    loop {
        seen.extend(worker.drain());
        if let Some(ExportEvent::Finished { .. }) = seen.last() {
            let last = seen.pop().unwrap();
            return (seen, last);
        }
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn photos_run_sequentially_in_order_with_progress() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let body: Arc<ExportBody> = {
        let order = Arc::clone(&order);
        Arc::new(move |_, item, _| {
            order.lock().unwrap().push(item.image.0);
            Ok(Outcome::Written)
        })
    };
    let worker = ExportWorker::start(body, Arc::new(|| {})).unwrap();
    worker.start_job(job(3)).unwrap();
    let (progress, last) = finished(&worker);
    assert_eq!(*order.lock().unwrap(), [1, 2, 3]);
    assert_eq!(progress.len(), 3);
    assert_eq!(
        progress[2],
        ExportEvent::Progress {
            done: 3,
            failed: 0,
            total: 3
        }
    );
    assert_eq!(
        last,
        ExportEvent::Finished {
            done: 3,
            failed: 0,
            first_error: None,
            dir: PathBuf::from("/out")
        }
    );
}

#[test]
fn a_failing_photo_is_counted_and_the_batch_continues() {
    let body: Arc<ExportBody> = Arc::new(|_, item, _| match item.image.0 {
        2 => Err("boom".into()),
        3 => panic!("kaput"),
        _ => Ok(Outcome::Written),
    });
    let worker = ExportWorker::start(body, Arc::new(|| {})).unwrap();
    worker.start_job(job(4)).unwrap();
    let (_, last) = finished(&worker);
    assert_eq!(
        last,
        ExportEvent::Finished {
            done: 2,
            failed: 2,
            first_error: Some("boom".into()),
            dir: PathBuf::from("/out")
        }
    );
}

#[test]
fn cancel_lets_the_current_photo_finish_and_skips_the_rest() {
    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(Mutex::new(Vec::new()));
    let body: Arc<ExportBody> = {
        let (started, release, ran) =
            (Arc::clone(&started), Arc::clone(&release), Arc::clone(&ran));
        Arc::new(move |_, item, _| {
            ran.lock().unwrap().push(item.image.0);
            started.store(true, Ordering::SeqCst);
            while !release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(Outcome::Written)
        })
    };
    let worker = ExportWorker::start(body, Arc::new(|| {})).unwrap();
    worker.start_job(job(5)).unwrap();
    while !started.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(1));
    }
    worker.cancel();
    release.store(true, Ordering::SeqCst);
    let (_, last) = finished(&worker);
    assert_eq!(*ran.lock().unwrap(), [1]);
    assert_eq!(
        last,
        ExportEvent::Finished {
            done: 1,
            failed: 0,
            first_error: None,
            dir: PathBuf::from("/out")
        }
    );
}

#[test]
fn a_new_job_after_a_cancelled_one_runs_normally() {
    let body: Arc<ExportBody> = Arc::new(|_, _, _| Ok(Outcome::Written));
    let worker = ExportWorker::start(body, Arc::new(|| {})).unwrap();
    worker.cancel();
    worker.start_job(job(2)).unwrap();
    let (_, last) = finished(&worker);
    assert!(matches!(last, ExportEvent::Finished { done: 2, .. }));
}

#[test]
fn open_doc_params_apply_to_the_open_image_only() {
    let edited = DevelopParams {
        exposure: 1.5,
        ..Default::default()
    };
    let open = (ImageId(1), Arc::new(edited.clone()));
    let stored = || {
        Ok(Some(DevelopParams {
            exposure: -2.0,
            ..Default::default()
        }))
    };
    let same = render::resolve_params(Some(&open), ImageId(1), || {
        panic!("stored edit must not be read for the open image")
    })
    .unwrap();
    assert_eq!(same, edited);
    let other = render::resolve_params(Some(&open), ImageId(2), stored).unwrap();
    assert_eq!(other.exposure, -2.0);
    let none = render::resolve_params(None, ImageId(2), || Ok(None)).unwrap();
    assert_eq!(none, DevelopParams::default());
    assert!(render::resolve_params(None, ImageId(2), || Err("bad".into())).is_err());
}
