//! Temporary host adapters over the shared, bounded executor.
use crate::state::AppState;
use core_app::jobs::{JobClass, JobFailure, JobOwner, JobSpec, QueuePolicy};
use core_runtime::jobs::JobContext;
use tauri::{AppHandle, Manager, Runtime};

/// Use for any operation that can fail; a failed inner result marks the job Failed.
pub async fn spawn_fallible<R, U, E, F>(
    app: AppHandle<R>,
    class: JobClass,
    durable: bool,
    operation: &'static str,
    task: F,
) -> Result<Result<U, E>, String>
where
    R: Runtime,
    U: Send + 'static,
    E: Send + 'static,
    F: FnOnce() -> Result<U, E> + Send + 'static,
{
    spawn_job(app, class, durable, operation, move |_| task()).await
}

/// For long operations with commit boundaries; call `mark_committed` after durable side effects.
pub async fn spawn_job<R, U, E, F>(
    app: AppHandle<R>,
    class: JobClass,
    durable: bool,
    operation: &'static str,
    task: F,
) -> Result<Result<U, E>, String>
where
    R: Runtime,
    U: Send + 'static,
    E: Send + 'static,
    F: FnOnce(&JobContext) -> Result<U, E> + Send + 'static,
{
    execute(app, class, durable, operation, move |context| {
        Ok(task(&context))
    })
    .await
}

pub async fn viewport_render<R: Runtime, T: Send + 'static>(
    app: AppHandle<R>,
    image_id: i64,
    task: impl FnOnce(u64) -> T + Send + 'static,
) -> Result<T, String> {
    let host = app.clone();
    match execute(
        app,
        JobClass::Viewport,
        false,
        "develop_render",
        move |context| {
            let st = host.state::<AppState>();
            let id = context.id().0;
            claim_viewport(
                &st.publications,
                &st.latest_render,
                &st.current_image,
                id,
                image_id,
            )?;
            Ok(Ok::<T, std::convert::Infallible>(task(id)))
        },
    )
    .await
    {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(unreachable)) => match unreachable {},
        Err(error) => Err(error),
    }
}

fn claim_viewport(
    gate: &core_runtime::publication::PublicationGate,
    latest: &std::sync::atomic::AtomicU64,
    current: &std::sync::atomic::AtomicI64,
    job_id: u64,
    image_id: i64,
) -> Result<(), JobFailure> {
    use std::sync::atomic::Ordering::SeqCst;
    let _admission = gate.mutation_guard();
    if latest.load(SeqCst) > job_id {
        return Err(JobFailure::Superseded);
    }
    latest.store(job_id, SeqCst);
    current.store(image_id, SeqCst);
    Ok(())
}

async fn execute<R, T, E, F>(
    app: AppHandle<R>,
    class: JobClass,
    durable: bool,
    operation: &'static str,
    task: F,
) -> Result<Result<T, E>, String>
where
    R: Runtime,
    T: Send + 'static,
    E: Send + 'static,
    F: FnOnce(JobContext) -> Result<Result<T, E>, JobFailure> + Send + 'static,
{
    let handle = {
        let st = app.state::<AppState>();
        let mut spec = spec(st.inner(), class, durable);
        if class == JobClass::Viewport {
            use std::hash::{Hash, Hasher};
            let mut key = std::collections::hash_map::DefaultHasher::new();
            operation.hash(&mut key);
            spec.policy = QueuePolicy::Latest { key: key.finish() };
        }
        let epoch = spec.owner.catalog_epoch;
        let host = app.clone();
        st.jobs
            .submit_fallible(spec, move |context| {
                context.checkpoint()?;
                if host.state::<AppState>().workers.is_stopping() && !durable {
                    return Err(JobFailure::Cancelled);
                }
                if host.state::<AppState>().publications.epoch() != epoch {
                    return Err(JobFailure::Superseded);
                }
                let result = task(context.clone())?;
                // A committed task already describes durable writes; rejecting its result
                // here would report failure for work that exists (e.g. an import whose
                // catalog was replaced after its rows committed). The worker records
                // `Interrupted` if a cancel was meanwhile requested.
                if !durable && !context.is_committed() {
                    context.checkpoint()?;
                    if host.state::<AppState>().publications.epoch() != epoch {
                        return Err(JobFailure::Superseded);
                    }
                }
                Ok(result)
            })
            .map_err(|e| format!("background queue: {e:?}"))?
    };
    handle.await.map_err(|e| format!("background job: {e:?}"))
}

pub fn spec(st: &AppState, class: JobClass, durable: bool) -> JobSpec {
    JobSpec {
        owner: JobOwner {
            catalog_epoch: st.publications.epoch(),
            editor_session: None,
        },
        class,
        policy: QueuePolicy::Ordered,
        // Scheduling reservation; native resource sizing remains the resource adapter's responsibility.
        estimated_bytes: if matches!(
            class,
            JobClass::Viewport
                | JobClass::Thumbnail
                | JobClass::Import
                | JobClass::Export
                | JobClass::Analysis
                | JobClass::Prefetch
        ) {
            512 * 1024 * 1024
        } else {
            64 * 1024
        },
        durable,
    }
}
pub fn cancel_class(st: &AppState, class: JobClass) {
    for job in st.jobs.snapshot() {
        if job.spec.class == class && !job.state.is_terminal() {
            st.jobs.cancel(job.id);
        }
    }
}
pub fn worker<R: Runtime>(
    app: &AppHandle<R>,
    name: &'static str,
    run: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    app.state::<AppState>().workers.spawn(name, run)
}
pub fn background<R: Runtime>(
    app: &AppHandle<R>,
    class: JobClass,
    run: impl FnOnce() + Send + 'static,
) {
    let st = app.state::<AppState>();
    let mut spec = spec(st.inner(), class, false);
    if class == JobClass::Prefetch {
        spec.policy = QueuePolicy::Latest { key: 0 };
    }
    if let Err(error) = st.jobs.submit(spec, move |context| {
        context.checkpoint()?;
        run();
        Ok(())
    }) {
        tracing::warn!(?error, "background request rejected");
    }
}

/// Stop producers before joining any consumer. No DB lock is held here.
pub fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    let st = app.state::<AppState>();
    st.workers.stop_producers();
    if let Ok(mut watcher) = st.watcher.lock() {
        watcher.take();
    }
    crate::scan::cancel(st.inner());
    for flag in [
        &st.panorama_cancel,
        &st.denoise_cancel,
        &st.hdr_cancel,
        &st.pano_detect_cancel,
        &st.analysis_cancel,
        &st.cluster_cancel,
        &st.analysis_dl_cancel,
        &st.faces_dl_cancel,
        &st.mask_ai_dl_cancel,
    ] {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    st.thumb_queue.shutdown();
    st.prefetch_queue.shutdown();
    st.jobs.stop();
    st.jobs.join();
    st.sidecar_queue.shutdown();
    // The mirroring worker owns intents that are durable in the catalog, so it may leave rows
    // queued; it only has to stop touching the disk and let itself be joined.
    st.outbox.shutdown();
    for error in st.workers.join() {
        tracing::error!(%error, "background worker failed");
    }
    let failures = st.jobs.worker_failures();
    if failures > 0 {
        tracing::error!(failures, "executor worker failed");
    }
}

pub fn cancelled(error: JobFailure) -> String {
    format!("background job: {error:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        mpsc, Arc,
    };

    #[test]
    fn delayed_older_worker_cannot_replace_newer_viewport_owner() {
        let pool = core_runtime::jobs::Supervisor::new(
            core_runtime::jobs::Config {
                max_jobs: 4,
                max_bytes: 40,
                max_terminal_records: 4,
                workers: 3,
            },
            Arc::new(|| {}),
        )
        .unwrap();
        let gate = Arc::new(core_runtime::publication::PublicationGate::default());
        let latest = Arc::new(AtomicU64::new(0));
        let current = Arc::new(AtomicI64::new(0));
        let spec = JobSpec {
            owner: JobOwner {
                catalog_epoch: gate.epoch(),
                editor_session: None,
            },
            class: JobClass::Viewport,
            policy: QueuePolicy::Latest { key: 1 },
            estimated_bytes: 10,
            durable: false,
        };
        let (started, observed) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let old_gate = gate.clone();
        let old_latest = latest.clone();
        let old_current = current.clone();
        let old = pool
            .submit(spec, move |context| {
                started.send(()).unwrap();
                proceed.recv().unwrap();
                claim_viewport(&old_gate, &old_latest, &old_current, context.id().0, 11)
            })
            .unwrap();
        observed
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let new_gate = gate.clone();
        let new_latest = latest.clone();
        let new_current = current.clone();
        let new = pool
            .submit(spec, move |context| {
                claim_viewport(&new_gate, &new_latest, &new_current, context.id().0, 22)
            })
            .unwrap();
        let outcome = new.wait();
        release.send(()).unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(old.wait(), Err(JobFailure::Superseded));
        assert_eq!(latest.load(Ordering::SeqCst), new.id().0);
        assert_eq!(current.load(Ordering::SeqCst), 22);
    }

    #[test]
    fn fallible_failure_is_recorded_failed_and_worker_survives() {
        use core_app::jobs::JobState;
        let pool = core_runtime::jobs::Supervisor::new(
            core_runtime::jobs::Config {
                max_jobs: 4,
                max_bytes: 40,
                max_terminal_records: 4,
                workers: 3,
            },
            Arc::new(|| {}),
        )
        .unwrap();
        let spec = JobSpec {
            owner: JobOwner {
                catalog_epoch: core_model::identity::CatalogEpoch(0),
                editor_session: None,
            },
            class: JobClass::UserCommand,
            policy: QueuePolicy::Ordered,
            estimated_bytes: 10,
            durable: false,
        };
        let failed = pool
            .submit_fallible(spec, |_| Ok(Err::<(), String>("x".into())))
            .unwrap();
        assert_eq!(failed.wait(), Ok(Err("x".to_string())));
        assert_eq!(failed.status().unwrap().state, JobState::Failed);
        let ok = pool.submit(spec, |_| Ok(5)).unwrap();
        assert_eq!(ok.wait(), Ok(5));
        assert_eq!(ok.status().unwrap().state, JobState::Succeeded);
    }
}
