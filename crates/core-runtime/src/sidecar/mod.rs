//! The sidecar-mirror half of the runtime, shared by every host: the fenced per-image write
//! decision ([`write`]), the durable outbox drain ([`outbox`]), and the thread that runs it for a
//! host whose catalog comes and goes ([`service::MirrorService`]). The Tauri-only worker spawn
//! stays in `src-tauri/src/outbox_worker.rs`; other hosts start a [`service::MirrorService`]
//! instead of calling [`outbox::OutboxHandle::run`] themselves.

pub mod outbox;
pub mod service;
pub mod write;

pub use outbox::{DrainReport, OutboxClock, OutboxHandle, SystemClock};
pub use service::MirrorService;
pub use write::{production_write, safe_write_error, write_one, SidecarOutcome};

/// Catalog fixture for host-side tests (the hosts' `sidecar_queue` tests reuse `write::tests::Fix`);
/// enabled by the `test-support` feature so it never lands in a production build.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    pub use super::write::tests::Fix;
}
