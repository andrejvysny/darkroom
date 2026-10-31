mod handle;
mod worker;

use core_app::jobs::{
    JobClass, JobFailure, JobId, JobProgress, JobSnapshot, JobSpec, JobState, Metrics, QueuePolicy,
    SubmitError,
};
pub use handle::JobHandle;
use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::Waker;
use std::thread::JoinHandle;

pub struct Config {
    pub max_jobs: usize,
    pub max_bytes: u64,
    pub max_terminal_records: usize,
    pub workers: usize,
}
type Payload = Box<dyn Any + Send>;
type Task = Box<dyn FnOnce(JobContext) -> Result<Payload, JobFailure> + Send>;
struct Record {
    snapshot: JobSnapshot,
    task: Option<Task>,
    result: Option<Result<Payload, JobFailure>>,
    charged: bool,
    handle: bool,
    waker: Option<Waker>,
    completion: Arc<Completion>,
}
struct Completion {
    snapshot: Mutex<Option<JobSnapshot>>,
    consumed: AtomicBool,
    terminal: Mutex<Option<(JobFailure, JobSnapshot)>>,
}
struct State {
    records: HashMap<JobId, Record>,
    queue: VecDeque<JobId>,
    next: u64,
    stopping: bool,
    reserved: u64,
    charged: usize,
    worker_failures: usize,
}
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    wake: Arc<dyn Fn() + Send + Sync>,
    config: Config,
}
pub struct Supervisor {
    shared: Arc<Shared>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}
#[derive(Clone)]
pub struct JobContext {
    shared: Arc<Shared>,
    id: JobId,
}
impl JobContext {
    pub fn id(&self) -> JobId {
        self.id
    }
    pub fn checkpoint(&self) -> Result<(), JobFailure> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.records.get(&self.id).map(|r| r.snapshot.state) {
            Some(JobState::Queued | JobState::Running) => Ok(()),
            Some(JobState::Superseded) | None => Err(JobFailure::Superseded),
            _ => Err(JobFailure::Cancelled),
        }
    }
    pub fn progress(&self, done: u64, total: u64) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = state.records.get_mut(&self.id) {
            let progress = Some(JobProgress { done, total });
            if record.snapshot.state.is_terminal() || record.snapshot.progress == progress {
                return;
            }
            record.snapshot.progress = progress;
        }
        drop(state);
        notify(&self.shared);
    }
}
impl Supervisor {
    pub fn new(config: Config, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, JobFailure> {
        if config.workers < 2 || config.max_jobs == 0 || config.max_terminal_records == 0 {
            return Err(JobFailure::InvalidConfig);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                records: HashMap::new(),
                queue: VecDeque::new(),
                next: 0,
                stopping: false,
                reserved: 0,
                charged: 0,
                worker_failures: 0,
            }),
            ready: Condvar::new(),
            wake,
            config,
        });
        let mut workers = Vec::new();
        for index in 0..shared.config.workers {
            let worker_shared = shared.clone();
            match std::thread::Builder::new()
                .name(format!("darkroom-job-{index}"))
                .spawn(move || worker::run(worker_shared, index == 0))
            {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    shared
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .stopping = true;
                    shared.ready.notify_all();
                    for handle in workers {
                        if handle.join().is_err() {
                            shared
                                .state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .worker_failures += 1;
                        }
                    }
                    return Err(JobFailure::WorkerStart(error.to_string()));
                }
            }
        }
        Ok(Self {
            shared,
            workers: Mutex::new(workers),
        })
    }
    pub fn submit<T: Send + 'static>(
        &self,
        spec: JobSpec,
        task: impl FnOnce(JobContext) -> Result<T, JobFailure> + Send + 'static,
    ) -> Result<JobHandle<T>, SubmitError> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping {
            return Err(SubmitError::Stopped);
        }
        let matching: Vec<JobId> = state
            .records
            .iter()
            .filter_map(|(id, r)| {
                let same = r.snapshot.spec.owner == spec.owner
                    && r.snapshot.spec.class == spec.class
                    && !r.snapshot.state.is_terminal();
                let keys = match (spec.policy, r.snapshot.spec.policy) {
                    (QueuePolicy::Latest { key: a }, QueuePolicy::Latest { key: b })
                    | (
                        QueuePolicy::Deduplicated { key: a },
                        QueuePolicy::Deduplicated { key: b },
                    ) => a == b,
                    _ => false,
                };
                (same && keys).then_some(*id)
            })
            .collect();
        if matches!(spec.policy, QueuePolicy::Deduplicated { .. }) && !matching.is_empty() {
            return Err(SubmitError::Duplicate(matching[0]));
        }
        let pending: Vec<_> = matching
            .iter()
            .copied()
            .filter(|id| {
                state.records[id].snapshot.state == JobState::Queued
                    && !state.records[id].snapshot.spec.durable
            })
            .collect();
        let reclaim_bytes: u64 = pending
            .iter()
            .map(|id| state.records[id].snapshot.spec.estimated_bytes)
            .sum();
        if state.charged - pending.len()
            >= self
                .shared
                .config
                .max_jobs
                .min(self.shared.config.max_terminal_records)
            || (state.reserved - reclaim_bytes)
                .checked_add(spec.estimated_bytes)
                .is_none_or(|n| n > self.shared.config.max_bytes)
        {
            // A rejected newer intent still invalidates obsolete disposable work.
            let mut retired = Vec::new();
            let mut wakers = Vec::new();
            for old in &matching {
                let record = state.records.get_mut(old).expect("matching job");
                if record.snapshot.spec.durable {
                    continue;
                }
                match record.snapshot.state {
                    JobState::Queued => {
                        retired.extend(record.task.take());
                        record.snapshot.state = JobState::Superseded;
                        *record
                            .completion
                            .terminal
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) =
                            Some((JobFailure::Superseded, record.snapshot.clone()));
                        *record
                            .completion
                            .snapshot
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(record.snapshot.clone());
                        wakers.extend(record.waker.take());
                    }
                    JobState::Running => record.snapshot.state = JobState::CancelRequested,
                    _ => {}
                }
            }
            for old in &pending {
                worker::uncharge(&mut state, *old);
            }
            let queue = std::mem::take(&mut state.queue);
            state.queue = queue
                .into_iter()
                .filter(|id| !matching_contains_terminal(&state.records, *id))
                .collect();
            drop(state);
            drop(retired);
            for waker in wakers {
                wake(waker);
            }
            self.shared.ready.notify_all();
            notify(&self.shared);
            return Err(SubmitError::Capacity);
        }
        let next = state.next.checked_add(1).ok_or(SubmitError::Exhausted)?;
        let id = JobId(next);
        state.next = next;
        let mut wakes = Vec::new();
        let mut retired_tasks = Vec::new();
        let mut retired_records = Vec::new();
        for old in matching {
            let record = state.records.get_mut(&old).unwrap();
            if record.snapshot.state == JobState::Queued && !record.snapshot.spec.durable {
                retired_tasks.extend(record.task.take());
                record.snapshot.state = JobState::Superseded;
                *record
                    .completion
                    .terminal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some((JobFailure::Superseded, record.snapshot.clone()));
                *record
                    .completion
                    .snapshot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(record.snapshot.clone());
                record.charged = false;

                if let Some(waker) = record.waker.take() {
                    wakes.push(waker);
                }
            } else if !record.snapshot.spec.durable {
                record.snapshot.state = JobState::CancelRequested;
            }
        }
        state.charged -= pending.len();
        state.reserved -= reclaim_bytes;
        let queue = std::mem::take(&mut state.queue);
        state.queue = queue
            .into_iter()
            .filter(|id| !matching_contains_terminal(&state.records, *id))
            .collect();
        let evict: Vec<_> = state
            .records
            .iter()
            .filter_map(|(id, r)| (r.snapshot.state.is_terminal() && !r.charged).then_some(*id))
            .collect();
        let mut evict = evict;
        evict.sort_by_key(|id| id.0);
        let excess = state
            .records
            .len()
            .saturating_add(1)
            .saturating_sub(self.shared.config.max_terminal_records);
        for old in evict.into_iter().take(excess) {
            retired_records.extend(state.records.remove(&old));
        }
        let completion = Arc::new(Completion {
            snapshot: Mutex::new(None),
            consumed: AtomicBool::new(false),
            terminal: Mutex::new(None),
        });
        state.reserved += spec.estimated_bytes;
        state.charged += 1;
        state.records.insert(
            id,
            Record {
                snapshot: JobSnapshot {
                    id,
                    spec,
                    state: JobState::Queued,
                    progress: None,
                },
                task: Some(Box::new(move |ctx| {
                    task(ctx).map(|value| Box::new(value) as Payload)
                })),
                result: None,
                charged: true,
                handle: true,
                waker: None,
                completion: completion.clone(),
            },
        );
        state.queue.push_back(id);
        drop(state);
        drop(retired_tasks);
        drop(retired_records);
        self.shared.ready.notify_all();
        for waker in wakes {
            wake(waker);
        }
        notify(&self.shared);
        Ok(JobHandle::new(self.shared.clone(), id, completion))
    }
    /// Wait outside catalog/publication locks; false means producer admission stopped.
    pub fn wait_capacity(&self, bytes: u64) -> bool {
        if bytes > self.shared.config.max_bytes {
            return false;
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while !state.stopping
            && (state.charged
                >= self
                    .shared
                    .config
                    .max_jobs
                    .min(self.shared.config.max_terminal_records)
                || state
                    .reserved
                    .checked_add(bytes)
                    .is_none_or(|n| n > self.shared.config.max_bytes))
        {
            state = self
                .shared
                .ready
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        !state.stopping
    }
    pub fn snapshot(&self) -> Vec<JobSnapshot> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .values()
            .map(|r| r.snapshot.clone())
            .collect()
    }
    pub fn metrics(&self) -> Metrics {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        Metrics {
            queued: state
                .records
                .values()
                .filter(|r| r.snapshot.state == JobState::Queued)
                .count(),
            running: state
                .records
                .values()
                .filter(|r| {
                    matches!(
                        r.snapshot.state,
                        JobState::Running | JobState::CancelRequested
                    )
                })
                .count(),
            retained: state
                .records
                .values()
                .filter(|r| r.snapshot.state.is_terminal())
                .count(),
            reserved_bytes: state.reserved,
        }
    }
    pub fn cancel(&self, id: JobId) -> bool {
        worker::cancel(&self.shared, id)
    }
    pub fn forget(&self, id: JobId) -> bool {
        handle::forget(&self.shared, id)
    }
    pub fn stop(&self) {
        worker::stop(&self.shared);
    }
    pub fn worker_failures(&self) -> usize {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker_failures
    }
    pub fn join(&self) {
        self.stop();
        let workers =
            { std::mem::take(&mut *self.workers.lock().unwrap_or_else(|e| e.into_inner())) };
        for worker in workers {
            if worker.join().is_err() {
                self.shared
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .worker_failures += 1;
            }
        }
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        self.join();
    }
}
fn matching_contains_terminal(records: &HashMap<JobId, Record>, id: JobId) -> bool {
    records
        .get(&id)
        .is_none_or(|r| r.snapshot.state.is_terminal())
}
fn priority(class: JobClass) -> u8 {
    match class {
        JobClass::UserCommand | JobClass::Viewport => 0,
        JobClass::Thumbnail => 1,
        JobClass::Import
        | JobClass::Export
        | JobClass::Analysis
        | JobClass::Sidecar
        | JobClass::Maintenance => 2,
        JobClass::Prefetch => 3,
    }
}

fn notify(shared: &Shared) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (shared.wake)()));
}

fn wake(waker: Waker) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker.wake()));
}
