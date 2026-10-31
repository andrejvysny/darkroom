use core_model::identity::{CatalogEpoch, EditorSession};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JobId(pub u64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobOwner {
    pub catalog_epoch: CatalogEpoch,
    pub editor_session: Option<EditorSession>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobClass {
    UserCommand,
    Viewport,
    Thumbnail,
    Import,
    Export,
    Analysis,
    Prefetch,
    Sidecar,
    Maintenance,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueuePolicy {
    Ordered,
    Latest { key: u64 },
    Deduplicated { key: u64 },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobSpec {
    pub owner: JobOwner,
    pub class: JobClass,
    pub policy: QueuePolicy,
    pub estimated_bytes: u64,
    pub durable: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    CancelRequested,
    Succeeded,
    Cancelled,
    Superseded,
    Failed,
}
impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Cancelled | Self::Superseded | Self::Failed
        )
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobFailure {
    Cancelled,
    Superseded,
    Failed(String),
    Panic,
    Consumed,
    InvalidConfig,
    WorkerStart(String),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubmitError {
    Stopped,
    Capacity,
    Duplicate(JobId),
    Exhausted,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobProgress {
    pub done: u64,
    pub total: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobSnapshot {
    pub id: JobId,
    pub spec: JobSpec,
    pub state: JobState,
    pub progress: Option<JobProgress>,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Metrics {
    pub queued: usize,
    pub running: usize,
    pub retained: usize,
    pub reserved_bytes: u64,
}
