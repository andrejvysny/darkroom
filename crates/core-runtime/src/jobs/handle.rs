use super::*;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct JobHandle<T> {
    shared: Arc<Shared>,
    id: JobId,
    completion: Arc<Completion>,
    marker: PhantomData<fn() -> T>,
}
impl<T: Send + 'static> JobHandle<T> {
    pub(super) fn new(shared: Arc<Shared>, id: JobId, completion: Arc<Completion>) -> Self {
        Self {
            shared,
            id,
            completion,
            marker: PhantomData,
        }
    }
    pub fn id(&self) -> JobId {
        self.id
    }
    pub fn status(&self) -> Option<JobSnapshot> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .records
            .get(&self.id)
            .map(|r| r.snapshot.clone())
            .or_else(|| {
                self.completion
                    .snapshot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            })
    }
    pub fn cancel(&self) -> bool {
        worker::cancel(&self.shared, self.id)
    }
    fn take_locked(&self, state: &mut State) -> Option<Result<T, JobFailure>> {
        if self.completion.consumed.load(Ordering::Acquire) {
            return Some(Err(JobFailure::Consumed));
        }
        let terminal = self
            .completion
            .terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let result = terminal.map(|(error, _)| Err(error)).or_else(|| {
            state
                .records
                .get_mut(&self.id)
                .and_then(|r| r.result.take())
        });
        result.map(|value| {
            self.completion.consumed.store(true, Ordering::Release);
            worker::uncharge(state, self.id);
            value.map(|payload| *payload.downcast::<T>().expect("job result type"))
        })
    }
    /// Returns Consumed after the single result has already been taken.
    pub fn try_take(&self) -> Option<Result<T, JobFailure>> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let result = self.take_locked(&mut state);
        drop(state);
        self.shared.ready.notify_all();
        result
    }
    pub fn wait(&self) -> Result<T, JobFailure> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(result) = self.take_locked(&mut state) {
                drop(state);
                self.shared.ready.notify_all();
                return result;
            }
            state = self
                .shared
                .ready
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}
impl<T: Send + 'static> Future for JobHandle<T> {
    type Output = Result<T, JobFailure>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let waker = cx.waker().clone();
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = self.take_locked(&mut state) {
            drop(state);
            drop(waker);
            self.shared.ready.notify_all();
            return Poll::Ready(result);
        }
        let old = state
            .records
            .get_mut(&self.id)
            .expect("pending job")
            .waker
            .replace(waker);
        drop(state);
        drop(old);
        Poll::Pending
    }
}
impl<T> Drop for JobHandle<T> {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut waker = None;
        let mut result = None;
        if let Some(record) = state.records.get_mut(&self.id) {
            record.handle = false;
            waker = record.waker.take();
            if record.snapshot.state.is_terminal() {
                result = record.result.take();
                worker::uncharge(&mut state, self.id);
            }
        }
        drop(state);
        drop(waker);
        drop(result);
        self.shared.ready.notify_all();
    }
}
pub(super) fn forget(shared: &Arc<Shared>, id: JobId) -> bool {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    if state
        .records
        .get(&id)
        .is_some_and(|r| r.snapshot.state.is_terminal() && !r.handle)
    {
        worker::uncharge(&mut state, id);
        let retired = state.records.remove(&id);
        drop(state);
        drop(retired);
        true
    } else {
        false
    }
}
