use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

pub(super) fn run(shared: Arc<Shared>, foreground: bool) {
    loop {
        let (id, task) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let candidate = state
                    .queue
                    .iter()
                    .enumerate()
                    .filter_map(|(i, id)| {
                        let record = state.records.get(id)?;
                        let p = priority(record.snapshot.spec.class);
                        (!foreground || p == 0 || state.stopping)
                            .then_some((i, p))
                            .filter(|_| {
                                record.snapshot.spec.policy != QueuePolicy::Ordered
                                    || !state.records.values().any(|active| {
                                        active.snapshot.spec.owner == record.snapshot.spec.owner
                                            && active.snapshot.spec.class
                                                == record.snapshot.spec.class
                                            && active.snapshot.spec.policy == QueuePolicy::Ordered
                                            && active.task.is_none()
                                            && matches!(
                                                active.snapshot.state,
                                                JobState::Running | JobState::CancelRequested
                                            )
                                    })
                            })
                    })
                    .min_by_key(|(_, p)| *p);
                if let Some((index, _)) = candidate {
                    let id = state.queue.remove(index).unwrap();
                    let record = state.records.get_mut(&id).unwrap();
                    if record.snapshot.state != JobState::CancelRequested {
                        record.snapshot.state = JobState::Running;
                    }
                    break (id, record.task.take().unwrap());
                }
                if state.stopping {
                    return;
                }
                state = shared.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        notify(&shared);
        let mut result = catch_unwind(AssertUnwindSafe(|| {
            task(JobContext {
                shared: shared.clone(),
                id,
            })
        }))
        .unwrap_or(Err(JobFailure::Panic));
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let record = state.records.get_mut(&id).unwrap();
        let mut retired = None;
        let (final_state, retire) = terminal(
            &result,
            record.snapshot.state == JobState::CancelRequested,
            record.committed,
            record.snapshot.spec.durable,
        );
        // Only an uncommitted, non-durable `Ok` result may be rewritten to `Cancelled`;
        // delivered values and typed errors always reach the handle untouched.
        if retire {
            retired = result.ok().map(|(payload, _)| payload);
            result = Err(JobFailure::Cancelled);
        }
        record.snapshot.state = final_state;
        *record
            .completion
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(record.snapshot.clone());
        record.result = Some(result.map(|(payload, _)| payload));
        let waker = record.waker.take();
        let abandoned = !record.handle;
        if abandoned {
            retired = record.result.take().and_then(Result::ok).or(retired);
            uncharge(&mut state, id);
        }
        drop(state);
        drop(retired);
        shared.ready.notify_all();
        if let Some(waker) = waker {
            wake(waker);
        }
        notify(&shared);
    }
}

/// Terminal mapping for a finished task. Returns the final state and whether the
/// delivered value may be retired. A delivered domain failure outranks a cancel
/// request: the typed error must still reach the handle, so it is never retired.
/// A committed job whose cancellation or supersession was honoured reports `Interrupted`
/// and keeps its payload, telling the caller work was retained instead of "nothing
/// happened" — the durable writes the commit describes outrank the late supersede.
fn terminal(
    result: &Result<(Payload, bool), JobFailure>,
    cancel_requested: bool,
    committed: bool,
    durable: bool,
) -> (JobState, bool) {
    match result {
        Ok((_, true)) => (JobState::Failed, false),
        Ok((_, false)) if cancel_requested => {
            if committed {
                (JobState::Interrupted, false)
            } else if !durable {
                (JobState::Cancelled, true)
            } else {
                (JobState::Succeeded, false)
            }
        }
        Ok(_) => (JobState::Succeeded, false),
        Err(JobFailure::Cancelled) if committed => (JobState::Interrupted, false),
        Err(JobFailure::Cancelled) => (JobState::Cancelled, false),
        Err(JobFailure::Superseded) if committed => (JobState::Interrupted, false),
        Err(JobFailure::Superseded) => (JobState::Superseded, false),
        Err(_) => (JobState::Failed, false),
    }
}
pub(super) fn uncharge(state: &mut State, id: JobId) {
    if let Some(record) = state.records.get_mut(&id) {
        if record.charged {
            record.charged = false;
            state.charged -= 1;
            state.reserved -= record.snapshot.spec.estimated_bytes;
        }
    }
}
pub(super) fn cancel(shared: &Arc<Shared>, id: JobId) -> bool {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(record) = state.records.get_mut(&id) else {
        return false;
    };
    let mut retired_task = None;
    let waker = match record.snapshot.state {
        JobState::Queued if !record.snapshot.spec.durable => {
            retired_task = record.task.take();
            record.snapshot.state = JobState::Cancelled;
            *record
                .completion
                .terminal
                .lock()
                .unwrap_or_else(|e| e.into_inner()) =
                Some((JobFailure::Cancelled, record.snapshot.clone()));
            *record
                .completion
                .snapshot
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(record.snapshot.clone());
            record.waker.take()
        }
        JobState::Queued | JobState::Running => {
            record.snapshot.state = JobState::CancelRequested;
            None
        }
        _ => return false,
    };
    if retired_task.is_some() {
        state.queue.retain(|queued| *queued != id);
        uncharge(&mut state, id);
    }
    drop(state);
    drop(retired_task);
    shared.ready.notify_all();
    if let Some(waker) = waker {
        wake(waker);
    }
    notify(shared);
    true
}
pub(super) fn stop(shared: &Arc<Shared>) {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.stopping {
        return;
    }
    state.stopping = true;
    let mut tasks = Vec::new();
    let mut wakers = Vec::new();
    let mut cancelled = Vec::new();
    for (id, record) in &mut state.records {
        if record.snapshot.spec.durable {
            continue;
        }
        match record.snapshot.state {
            JobState::Queued => {
                tasks.extend(record.task.take());
                record.snapshot.state = JobState::Cancelled;
                *record
                    .completion
                    .terminal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some((JobFailure::Cancelled, record.snapshot.clone()));
                *record
                    .completion
                    .snapshot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(record.snapshot.clone());
                wakers.extend(record.waker.take());
                cancelled.push(*id);
            }
            JobState::Running => record.snapshot.state = JobState::CancelRequested,
            _ => {}
        }
    }
    state.queue.retain(|id| !cancelled.contains(id));
    for id in cancelled {
        uncharge(&mut state, id);
    }
    drop(state);
    drop(tasks);
    shared.ready.notify_all();
    for waker in wakers {
        wake(waker);
    }
    notify(shared);
}
