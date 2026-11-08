//! Owned lifecycle for the few long-lived host adapters (watchers and coalescing producers).
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerState {
    Running,
    Completed,
    Failed(String),
}
#[derive(Clone)]
struct Entry {
    name: &'static str,
    state: Arc<Mutex<WorkerState>>,
}
struct State {
    stopping: bool,
    starting: usize,
    handles: Vec<JoinHandle<()>>,
    entries: Vec<Entry>,
}
pub struct ManagedWorkers {
    state: Mutex<State>,
    ready: Condvar,
    capacity: usize,
}
impl ManagedWorkers {
    pub fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State {
                stopping: false,
                starting: 0,
                handles: Vec::new(),
                entries: Vec::new(),
            }),
            ready: Condvar::new(),
            capacity,
        }
    }
    pub fn spawn(
        &self,
        name: &'static str,
        task: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<(), String> {
        self.reap();
        let report = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping {
                return Err("worker admission stopped".into());
            }
            if state.handles.len() + state.starting >= self.capacity {
                return Err("worker capacity reached".into());
            }
            state.starting += 1;
            let report = Arc::new(Mutex::new(WorkerState::Running));
            state.entries.push(Entry {
                name,
                state: report.clone(),
            });
            report
        };
        let result = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let outcome = catch_unwind(AssertUnwindSafe(task));
                let value = match outcome {
                    Ok(Ok(())) => WorkerState::Completed,
                    Ok(Err(error)) => WorkerState::Failed(error),
                    Err(_) => WorkerState::Failed("worker panicked".into()),
                };
                *report.lock().unwrap_or_else(|e| e.into_inner()) = value;
            });
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.starting -= 1;
        match result {
            Ok(handle) => state.handles.push(handle),
            Err(error) => {
                let message = error.to_string();
                if let Some(entry) = state.entries.iter().rev().find(|e| e.name == name) {
                    *entry.state.lock().unwrap_or_else(|e| e.into_inner()) =
                        WorkerState::Failed(message.clone());
                }
                self.ready.notify_all();
                return Err(message);
            }
        }
        self.ready.notify_all();
        Ok(())
    }
    fn reap(&self) {
        let retired = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let all = std::mem::take(&mut state.handles);
            let (finished, active): (Vec<_>, Vec<_>) =
                all.into_iter().partition(|handle| handle.is_finished());
            state.handles = active;
            if state.entries.len() > self.capacity.saturating_mul(2) {
                state.entries.retain(|entry| {
                    matches!(
                        *entry.state.lock().unwrap_or_else(|e| e.into_inner()),
                        WorkerState::Running
                    )
                });
            }
            finished
        };
        for handle in retired {
            let _ = handle.join();
        }
    }
    /// Phase initialization cannot clear cancellation after shutdown has begun.
    pub fn reset_cancel(&self, flag: &std::sync::atomic::AtomicBool) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping {
            return Err("application is shutting down".into());
        }
        flag.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    pub fn stop_producers(&self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping = true;
    }
    pub fn is_stopping(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping
    }
    pub fn snapshot(&self) -> Vec<(&'static str, WorkerState)> {
        let entries = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clone();
        entries
            .into_iter()
            .map(|entry| {
                (
                    entry.name,
                    entry
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone(),
                )
            })
            .collect()
    }
    /// Call after signalling every retained adapter's stop condition.
    pub fn join(&self) -> Vec<String> {
        let handles = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopping = true;
            while state.starting != 0 {
                state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            std::mem::take(&mut state.handles)
        };
        let mut errors = Vec::new();
        for handle in handles {
            if handle.join().is_err() {
                errors.push("worker join panicked".into());
            }
        }
        errors.extend(
            self.snapshot()
                .into_iter()
                .filter_map(|(name, state)| match state {
                    WorkerState::Failed(error) => Some(format!("{name}: {error}")),
                    _ => None,
                }),
        );
        errors
    }
}

impl Drop for ManagedWorkers {
    fn drop(&mut self) {
        self.stop_producers();
        let _ = self.join();
    }
}
