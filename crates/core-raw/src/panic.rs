//! Decoder-panic containment.
//!
//! rawler is a best-effort reverse-engineered decoder collection: it reaches `todo!()` for CFA
//! layouts it cannot demosaic, `unimplemented!()` for the ARW6 decompressor, and index-out-of-bounds
//! on some malformed files. Any one of those on a background indexing thread would take the whole
//! app down. Every public entry point in this crate that can reach rawler therefore runs inside
//! [`catch_decode_panic`], which turns the unwind into a typed [`RawError::DecoderPanic`] so ONE
//! bad file is skipped instead of aborting the process.
//!
//! Containment only works with unwinding panics — the workspace release profile deliberately does
//! NOT set `panic = "abort"`. [`ISOLATION_ACTIVE`] reports whether this build actually has it, so
//! the app can log a warning instead of silently losing the guarantee.

use std::cell::Cell;
use std::panic::AssertUnwindSafe;

use crate::error::RawError;

/// `true` when this build unwinds (so [`catch_decode_panic`] can actually catch). `false` under a
/// `panic = "abort"` profile, where a decoder panic still kills the process.
pub const ISOLATION_ACTIVE: bool = cfg!(panic = "unwind");

thread_local! {
    /// Number of decode entries currently on THIS thread's stack. A counter (not a flag) because
    /// the public entry points legitimately nest — `develop_linear_preview` falls back to
    /// `develop_linear`, `thumbnail_jpeg` calls both.
    ///
    /// Thread-local, not process-global, on purpose: the panic hook runs on the panicking thread,
    /// and only a panic raised on a thread that is inside `catch_decode_panic` is guaranteed to be
    /// caught by it. A global counter would make the hook mis-label every unrelated panic (UI, DB)
    /// as "contained" whenever a library index happens to be running. The price: a panic inside one
    /// of rawler's own rayon workers is still caught (rayon re-raises it on our thread) but is
    /// logged by the hook as a real panic with a backtrace — noisy, never wrong.
    static IN_FLIGHT: Cell<usize> = const { Cell::new(0) };
}

/// Whether the CURRENT thread is inside a RAW decode entry point. The process panic hook consults
/// this to log a contained decoder panic as a skipped file rather than a crash.
pub fn decode_in_flight() -> bool {
    IN_FLIGHT.with(|c| c.get() > 0)
}

/// RAII counter so the decrement survives the unwind that `catch_unwind` is about to absorb —
/// a bare decrement after the call would be skipped for the panic case and leave the thread
/// permanently "decoding".
struct InFlight;

impl InFlight {
    fn enter() -> Self {
        IN_FLIGHT.with(|c| c.set(c.get() + 1));
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Run `f`, converting a panic inside it into [`RawError::DecoderPanic`] tagged with `what`.
///
/// `AssertUnwindSafe` is sound here because nothing observable survives the unwind: the only shared
/// state is the `IN_FLIGHT` counter (restored by the guard) and the caller's `&RawSource`, which is
/// read-only. Anything the decoder half-built is dropped with its stack frame.
pub(crate) fn catch_decode_panic<T>(
    what: &str,
    f: impl FnOnce() -> Result<T, RawError>,
) -> Result<T, RawError> {
    let _guard = InFlight::enter();
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            Err(RawError::DecoderPanic(format!("{what}: {msg}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `IN_FLIGHT` is thread-local, so this test can assert the full lifecycle on its own thread:
    /// counted while inside, nested frames keep the outer count, and the decrement survives the
    /// unwind that `catch_unwind` absorbs.
    #[test]
    fn panic_becomes_typed_error_and_nests() {
        assert!(!decode_in_flight(), "fresh test thread must start idle");
        // The default hook prints the deliberate panics below to stderr; silence it meanwhile.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let err = catch_decode_panic("unit", || -> Result<(), RawError> {
            panic!("not implemented");
        })
        .expect_err("panic must surface as an error");
        assert!(matches!(err, RawError::DecoderPanic(_)));
        assert!(err.to_string().contains("unit: not implemented"), "{err}");

        // Nesting: the inner catch absorbs the panic; the outer frame is still counted afterwards,
        // which is what makes the counter (rather than a flag) load-bearing.
        let nested = catch_decode_panic("outer", || {
            assert!(decode_in_flight(), "own frame must be counted");
            let inner = catch_decode_panic("inner", || -> Result<(), RawError> { panic!("boom") });
            assert!(
                decode_in_flight(),
                "inner frame ending must not clear the outer one"
            );
            inner
        });
        assert!(matches!(nested, Err(RawError::DecoderPanic(_))));
        assert!(
            !decode_in_flight(),
            "guards must unwind cleanly — thread would otherwise stay 'decoding' forever"
        );

        std::panic::set_hook(previous);
    }

    #[test]
    fn ok_path_is_transparent() {
        let v = catch_decode_panic("unit", || Ok(41 + 1)).expect("no panic");
        assert_eq!(v, 42);
    }

    #[test]
    fn isolation_is_active_in_test_profile() {
        assert!(
            ISOLATION_ACTIVE,
            "tests run under `panic = \"abort\"` — decoder-panic containment is not being exercised"
        );
    }
}
