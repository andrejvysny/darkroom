//! render-wgpu — leased GPU frame targets and stale-safe presentation.
//!
//! [`Renderer`] develops images into pooled `Rgba8Unorm` GPU textures and hands them out as
//! [`FrameLease`]s; while a lease is alive the pool never hands its texture to another render.
//! [`Presenter`] is the UI-side owner of "what is on screen": it only publishes a finished
//! render whose [`FrameKey`] still matches what the UI wants, so a stale frame (superseded
//! image/session/viewport, retired device, older edit revision) can never replace a newer one.
//!
//! No egui types live here (egui texture registration happens in the host), and `render` never
//! reads pixels back to the CPU — [`Renderer::read_back`] is explicit, for tests/diagnostics.

mod pool;
mod presenter;
mod renderer;

pub use pool::{FrameLease, PoolLimits, TexturePool, FRAME_USAGE};
pub use presenter::{Offer, Presenter, StaleReason};
pub use renderer::{RenderError, Renderer};

pub use core_model::identity::{FrameHandle, FrameKey, QualityTier};
