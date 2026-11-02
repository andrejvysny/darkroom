use crate::catalog::CatalogAdapter;
use crate::jobs::{Config, JobHandle, Supervisor};
use crate::workflow::RuntimePaths;
use core_app::jobs::{JobClass, JobFailure, JobOwner, JobSnapshot, JobSpec, Metrics, QueuePolicy};
use core_app::ports::{CatalogPort, Clock, EffectExecutor, EventSink, RenderBackend};
use core_app::workflow::{Command, Completion, Effect, Operation};
use core_app::{AppError, ApplicationSession};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

// A nominal reservation: SQL result sizes are not bounded by row count alone.
const QUERY_BYTES: u64 = 16 * 1024 * 1024;
const PENDING_LIMIT: usize = 8;

type WakeState = Arc<(Mutex<bool>, Condvar)>;

struct Active {
    effect: Effect,
    handle: JobHandle<Result<Completion, Completion>>,
}

/// Owns application state on the caller thread; catalog effects execute serially on workers.
pub struct AsyncCatalogWorkflow {
    session: ApplicationSession,
    clock: Arc<dyn Clock>,
    paths: RuntimePaths,
    catalog: Arc<CatalogAdapter>,
    executor: Arc<dyn EffectExecutor>,
    supervisor: Supervisor,
    active: Option<Active>,
    pending: VecDeque<Effect>,
    wake_state: WakeState,
    wake_pending: Arc<AtomicBool>,
    shutdown_joined: bool,
}

impl AsyncCatalogWorkflow {
    pub fn new(
        paths: RuntimePaths,
        clock: Arc<dyn Clock>,
        events: Arc<dyn EventSink>,
        renderer: Arc<dyn RenderBackend>,
        executor: Arc<dyn EffectExecutor>,
        wake: Arc<dyn Fn() + Send + Sync>,
        config: Config,
    ) -> Result<Self, AppError> {
        if config.max_bytes < QUERY_BYTES {
            return Err(AppError::InvalidCommand(
                "job budget must accommodate the nominal query reservation".into(),
            ));
        }
        let wake_state: WakeState = Arc::new((Mutex::new(false), Condvar::new()));
        let wake_pending = Arc::new(AtomicBool::new(false));
        let external_pending = wake_pending.clone();
        let signal = wake_state.clone();
        let notify = Arc::new(move || {
            let (lock, condvar) = &*signal;
            let mut notified = lock.lock().unwrap_or_else(|e| e.into_inner());
            *notified = true;
            condvar.notify_all();
            drop(notified);
            if !external_pending.swap(true, Ordering::AcqRel) {
                wake();
            }
        });
        let supervisor = Supervisor::new(config, notify).map_err(job_error)?;
        Ok(Self {
            session: ApplicationSession::new(clock.clone(), events, renderer),
            clock,
            paths,
            catalog: Arc::new(CatalogAdapter::default()),
            executor,
            supervisor,
            active: None,
            pending: VecDeque::new(),
            wake_state,
            wake_pending,
            shutdown_joined: false,
        })
    }

    fn refresh_jobs(&mut self) -> Result<(), AppError> {
        self.session
            .update_jobs(self.supervisor.snapshot(), self.supervisor.metrics())?;
        Ok(())
    }
    pub fn cancel_job(&mut self, id: core_app::jobs::JobId) -> Result<bool, AppError> {
        let cancelled = self.supervisor.cancel(id);
        self.refresh_jobs()?;
        Ok(cancelled)
    }
    /// The catalog adapter, for host services that run outside the ordered lane (import).
    pub fn catalog(&self) -> Arc<CatalogAdapter> {
        Arc::clone(&self.catalog)
    }
    pub fn snapshot(&self) -> Arc<core_app::state::ApplicationSnapshot> {
        self.session.snapshot()
    }
    pub fn subscribe(&mut self) -> core_app::subscription::Subscription {
        self.session.subscribe()
    }
    pub fn job_snapshots(&self) -> Vec<JobSnapshot> {
        self.supervisor.snapshot()
    }
    pub fn metrics(&self) -> Metrics {
        self.supervisor.metrics()
    }
    pub fn open_initial(&mut self) -> Result<(), AppError> {
        self.dispatch(Command::OpenCatalog(self.paths.catalog.clone()))
    }

    pub fn dispatch(&mut self, command: Command) -> Result<(), AppError> {
        self.prune();
        if !self.can_admit(&command) {
            return Err(AppError::InvalidCommand(
                "catalog effect queue is full".into(),
            ));
        }
        let effects = self.session.dispatch(command)?;
        self.prune();
        if let Some(active) = &self.active {
            if !self.session.is_current(&active.effect) {
                active.handle.cancel();
            }
        }
        self.pending.extend(effects);
        self.start_next()?;
        self.refresh_jobs()?;
        Ok(())
    }

    fn can_admit(&self, command: &Command) -> bool {
        if matches!(
            command,
            Command::OpenCatalog(_)
                | Command::CloseCatalog
                | Command::RequestShutdown
                | Command::SelectImage(None)
                | Command::Edit(_)
        ) {
            return true;
        }
        let replaces = self.pending.iter().any(|effect| match command {
            Command::QueryLibrary(_) | Command::RefreshLibrary => {
                matches!(effect.operation, Operation::QueryLibrary(_))
            }
            Command::LoadFolders => matches!(effect.operation, Operation::LoadFolders),
            Command::LoadDates => matches!(effect.operation, Operation::LoadDates),
            Command::LoadKeywords => matches!(effect.operation, Operation::LoadKeywords),
            Command::LoadCollections => matches!(effect.operation, Operation::LoadCollections),
            Command::LoadImageTags(_) => matches!(effect.operation, Operation::LoadImageTags(_)),
            Command::LoadRejectSummary(_) => {
                matches!(effect.operation, Operation::LoadRejectSummary(_))
            }
            Command::SelectImage(Some(_)) => {
                matches!(effect.operation, Operation::SelectImage { .. })
            }
            _ => false,
        });
        replaces || self.pending.len() < PENDING_LIMIT
    }

    fn prune(&mut self) {
        self.pending
            .retain(|effect| self.session.is_current(effect));
    }

    fn start_next(&mut self) -> Result<(), AppError> {
        if self.active.is_some() {
            return Ok(());
        }
        self.prune();
        let Some(effect) = self.pending.pop_front() else {
            return Ok(());
        };
        let durable = matches!(
            effect.operation,
            Operation::OpenCatalog(_)
                | Operation::CloseCatalog
                | Operation::SaveEdit { .. }
                | Operation::SetCull { .. }
                | Operation::Mutate(_)
        );
        let spec = JobSpec {
            owner: JobOwner {
                catalog_epoch: effect.token.catalog_epoch,
                editor_session: None,
            },
            class: JobClass::UserCommand,
            policy: QueuePolicy::Ordered,
            estimated_bytes: if durable { 0 } else { QUERY_BYTES },
            durable,
        };
        let task_effect = effect.clone();
        let catalog = self.catalog.clone();
        let executor = self.executor.clone();
        match self.supervisor.submit_fallible(spec, move |ctx| {
            ctx.checkpoint()?;
            let completion = executor.execute(task_effect, catalog.as_ref());
            Ok(if completion.result.is_err() {
                Err(completion)
            } else {
                Ok(completion)
            })
        }) {
            Ok(handle) => {
                self.active = Some(Active { effect, handle });
                Ok(())
            }
            Err(error) => {
                self.pending.push_front(effect);
                Err(AppError::Catalog(
                    format!("job submission failed: {error:?}").into(),
                ))
            }
        }
    }

    /// Processes only completed work; never waits for catalog IO or worker shutdown.
    pub fn pump(&mut self) -> Result<bool, AppError> {
        self.wake_pending.store(false, Ordering::Release);
        self.tick_autosave()?;
        let Some(result) = self
            .active
            .as_ref()
            .and_then(|active| active.handle.try_take())
        else {
            self.start_next()?;
            self.refresh_jobs()?;
            return Ok(false);
        };
        let active = self.active.take().expect("completed active effect");
        if self.session.is_current(&active.effect) {
            let completion = match result {
                Ok(Ok(completion)) | Ok(Err(completion)) => completion,
                Err(failure) => Completion {
                    token: active.effect.token,
                    result: Err(job_error(failure)),
                },
            };
            self.pending.extend(self.session.complete(completion)?);
        }
        self.prune();
        self.start_next()?;
        self.refresh_jobs()?;
        Ok(true)
    }

    /// Earliest clock time (ms, the injected `Clock`'s axis) at which `pump` would issue an
    /// autosave; the host schedules a wake for it. `None` when nothing is waiting on the debounce.
    pub fn next_deadline(&self) -> Option<u64> {
        self.session.next_deadline()
    }

    fn tick_autosave(&mut self) -> Result<(), AppError> {
        let effects = self.session.tick(self.clock.now_millis())?;
        self.pending.extend(effects);
        Ok(())
    }

    /// Blocks the calling thread until all effects complete; intended for headless hosts.
    pub fn wait_idle(&mut self) -> Result<(), AppError> {
        loop {
            let signal = self.wake_state.clone();
            let (lock, condvar) = &*signal;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = false;
            while self.pump()? {}
            if self.active.is_none() && self.pending.is_empty() {
                return Ok(());
            }
            let notified = lock.lock().unwrap_or_else(|e| e.into_inner());
            drop(
                condvar
                    .wait_while(notified, |current| !*current)
                    .unwrap_or_else(|e| e.into_inner()),
            );
        }
    }

    pub fn needs_join(&self) -> bool {
        !self.session.snapshot().running
            && self.active.is_none()
            && self.pending.is_empty()
            && !self.shutdown_joined
    }
    pub fn join_shutdown(&mut self) -> bool {
        if !self.needs_join() {
            return false;
        }
        self.supervisor.join();
        self.shutdown_joined = true;
        true
    }
    pub fn is_shutdown_complete(&self) -> bool {
        self.shutdown_joined
    }
}

impl Drop for AsyncCatalogWorkflow {
    fn drop(&mut self) {
        self.supervisor.join();
        let _ = self.catalog.close();
    }
}

fn job_error(error: JobFailure) -> AppError {
    AppError::Catalog(format!("catalog job failed: {error:?}").into())
}
