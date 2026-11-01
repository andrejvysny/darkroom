use crate::{
    gpu::Gpu,
    paint,
    photo_worker::{self, PhotoRequest, PhotoResult, PhotoWorker},
    present,
    surface_state::{AcquireEvent, Action, SurfaceFailure, SurfaceState},
};
use core_app::state::{ApplicationSnapshot, DocumentStatus};
use core_model::identity::{DeviceEpoch, FrameKey};
use render_wgpu::{Offer, Presenter};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

pub fn run() -> Result<(), String> {
    run_with_catalog(None)
}

pub fn run_with_catalog(catalog_path: Option<std::path::PathBuf>) -> Result<(), String> {
    let event_loop = EventLoop::<Option<Instant>>::with_user_event()
        .build()
        .map_err(|e| e.to_string())?;
    let mut app = App {
        state: None,
        catalog_path,
        error: None,
        next_repaint: None,
        proxy: event_loop.create_proxy(),
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    app.error.map_or(Ok(()), Err)
}

struct App {
    catalog_path: Option<std::path::PathBuf>,
    state: Option<State>,
    error: Option<String>,
    next_repaint: Option<Instant>,
    proxy: EventLoopProxy<Option<Instant>>,
}

struct State {
    application: Option<crate::application::NativeApplication>,
    /// Native preferences (opened at startup from the app-data directory, legacy web bridge
    /// imported once) plus the notices the status area shows.
    settings: crate::settings::SettingsState,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    egui: egui::Context,
    input: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    size: winit::dpi::PhysicalSize<u32>,
    surface_state: SurfaceState,
    /// Last `application.dispatch` error, shown dismissibly in the UI; cleared
    /// on the next successful dispatch.
    last_command_error: Option<(String, Instant)>,
    /// Human-readable terminal surface failure; once set, acquisition stops.
    fatal: Option<String>,
    /// Whether the terminal shutdown has already been requested.
    shutdown_requested: bool,
    /// Decode/develop worker on the shared device (joined when `State` drops).
    photo: PhotoWorker,
    /// Device epoch the worker's renderer (and every wanted key) was built on.
    device_epoch: DeviceEpoch,
    /// Owner of "the frame on screen" — holds the presented lease's texture alive.
    presenter: Presenter,
    /// egui registration of the presented frame, kept together with the lease's handle: the
    /// texture can never be recycled by the pool while egui may still sample it, because the
    /// presenter holds the lease until it is replaced (and we free the id the moment the
    /// presenter no longer holds the matching lease).
    photo_frame: Option<(egui::TextureId, render_wgpu::FrameHandle, [u32; 2])>,
    /// Last frame key submitted to the presenter/worker.
    last_wanted: Option<FrameKey>,
    /// Last worker failure for the wanted key; shown as a label with the filename.
    photo_error: Option<(FrameKey, String)>,
}

fn sdr_config(
    surface: &wgpu::Surface<'_>,
    adapter: &wgpu::Adapter,
    size: winit::dpi::PhysicalSize<u32>,
) -> Result<wgpu::SurfaceConfiguration, String> {
    let mut config = surface
        .get_default_config(adapter, size.width.max(1), size.height.max(1))
        .ok_or("surface unsupported")?;
    let caps = surface.get_capabilities(adapter);
    config.format = caps
        .formats
        .iter()
        .copied()
        .find(|format| {
            matches!(
                format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            ) && caps
                .color_spaces(*format)
                .contains(wgpu::SurfaceColorSpaces::SRGB)
        })
        .ok_or("surface has no supported SDR format")?;
    config.color_space = wgpu::SurfaceColorSpace::Srgb;
    Ok(config)
}

impl State {
    fn new(
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<Option<Instant>>,
        catalog_path: Option<std::path::PathBuf>,
    ) -> Result<Self, String> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes().with_title(if catalog_path.is_some() {
                        "Darkroom — Catalog"
                    } else {
                        "Darkroom — Native stack qualification"
                    }),
                )
                .map_err(|e| e.to_string())?,
        );
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let gpu = Gpu::new(instance, Some(&surface))?;
        gpu.develop_probe()?;
        let size = window.inner_size();
        let config = sdr_config(&surface, &gpu.adapter, size)?;
        if size.width > 0 && size.height > 0 {
            surface.configure(&gpu.context.device, &config);
        }
        let (egui, input) = egui_input(event_loop, &window, &gpu.context, proxy.clone());
        let renderer =
            egui_wgpu::Renderer::new(&gpu.context.device, config.format, Default::default());
        // Settings load before the catalog: preferences are the host's, not the catalog's, and a
        // failure there must never stop the window from opening.
        let settings = crate::settings::SettingsState::load(catalog_path.as_deref());
        let wake_workflow = proxy.clone();
        let application = catalog_path
            .map(|path| {
                crate::application::NativeApplication::new(
                    path,
                    true,
                    Arc::new(move || {
                        let _ = wake_workflow.send_event(Some(Instant::now()));
                    }),
                )
            })
            .transpose()
            .map_err(|e| e.to_string())?;
        let photo_proxy = proxy;
        let device_epoch = DeviceEpoch(1);
        let photo = PhotoWorker::start(
            &gpu.context,
            Arc::clone(&gpu.pipeline),
            device_epoch,
            Arc::new(move || {
                let _ = photo_proxy.send_event(Some(Instant::now()));
            }),
        );
        Ok(Self {
            application,
            settings,
            window,
            surface,
            config,
            gpu,
            egui,
            input,
            renderer,
            size,
            surface_state: SurfaceState::new(),
            last_command_error: None,
            fatal: None,
            shutdown_requested: false,
            photo,
            device_epoch,
            presenter: Presenter::new(),
            photo_frame: None,
            last_wanted: None,
            photo_error: None,
        })
    }

    fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        self.size = size;
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface
            .configure(&self.gpu.context.device, &self.config);
        self.window.request_redraw();
    }

    fn recover_surface(&mut self) -> Result<(), String> {
        let surface = self
            .gpu
            ._instance
            .create_surface(self.window.clone())
            .map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&self.gpu.adapter);
        if !caps.formats.contains(&self.config.format)
            || !caps
                .color_spaces(self.config.format)
                .contains(wgpu::SurfaceColorSpaces::SRGB)
        {
            return Err("recreated surface does not support selected SDR format".into());
        }
        self.surface = surface;
        self.resize(self.size);
        Ok(())
    }

    /// Classify one acquisition attempt; the frame is only `Some` for
    /// `Presented`/`Suboptimal`. Policy lives in [`SurfaceState`].
    fn acquire(&self) -> (AcquireEvent, Option<wgpu::SurfaceTexture>, bool) {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => {
                (AcquireEvent::Presented, Some(frame), false)
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                (AcquireEvent::Suboptimal, Some(frame), true)
            }
            wgpu::CurrentSurfaceTexture::Outdated => (AcquireEvent::Outdated, None, false),
            wgpu::CurrentSurfaceTexture::Lost => (AcquireEvent::Lost, None, false),
            wgpu::CurrentSurfaceTexture::Timeout => (AcquireEvent::Timeout, None, false),
            wgpu::CurrentSurfaceTexture::Occluded => (AcquireEvent::Occluded, None, false),
            wgpu::CurrentSurfaceTexture::Validation => (AcquireEvent::Validation, None, false),
        }
    }

    /// One redraw pass; returns the delay before the next scheduled redraw.
    /// Never returns without either presenting or scheduling a bounded retry:
    /// a wedged surface backs off exponentially and goes fatal instead of
    /// busy-looping.
    fn redraw(&mut self) -> Duration {
        if self.fatal.is_some() {
            return Duration::MAX;
        }
        // An uncaptured wgpu device error (recorded by the `gpu.rs` hook instead of the default
        // handler aborting mid-frame) is terminal like a surface failure: route it through the
        // same fatal path so the application shuts down cleanly.
        if let Some(message) = self.gpu.errors.take() {
            self.fail(SurfaceFailure::UncapturedGpu(message));
            return Duration::MAX;
        }
        if self.size.width == 0 || self.size.height == 0 {
            self.surface_state.on_event(AcquireEvent::ZeroSize);
            return Duration::MAX;
        }
        let (event, frame, suboptimal) = self.acquire();
        match self.surface_state.on_event(event) {
            Action::Present => self.present(
                frame.expect("Presented/Suboptimal always carry a frame"),
                suboptimal,
            ),
            Action::Reconfigure { after } => {
                self.resize(self.size);
                after
            }
            Action::RecreateSurface { after } => match self.recover_surface() {
                Ok(()) => after,
                Err(message) => match self.surface_state.on_recreate_failed(message) {
                    Action::Fatal(failure) => {
                        self.fail(failure);
                        Duration::MAX
                    }
                    Action::Retry { after } => after,
                    _ => after,
                },
            },
            Action::WaitForVisibility | Action::Suspend => Duration::MAX,
            Action::Retry { after } => after,
            Action::Fatal(failure) => {
                self.fail(failure);
                Duration::MAX
            }
        }
    }

    /// Publish finished worker frames: `offer` through the presenter, registering the lease's
    /// texture with egui through the shared [`present::register_presented_frame`] (immediately
    /// freed for a lease the presenter rejects); on `Presented` the previous registration is
    /// freed afterwards, so a frame is never unregistered while it (or its successor) is on
    /// screen.
    fn drain_photo_results(&mut self) {
        for result in self.photo.drain_results() {
            match result {
                PhotoResult::Frame(lease) => {
                    let handle = lease.handle();
                    // Same registration helper `--photo-probe` runs; a lease the presenter
                    // rejects (stale key) hands its id straight back.
                    let (texture, size) = present::register_presented_frame(
                        &self.gpu.context.device,
                        &mut self.renderer,
                        &lease,
                    );
                    if self.presenter.offer(lease, self.device_epoch) == Offer::Presented {
                        if let Some((previous, _, _)) =
                            self.photo_frame.replace((texture, handle, size))
                        {
                            self.renderer.free_texture(&previous);
                        }
                    } else {
                        self.renderer.free_texture(&texture);
                    }
                    // Stale leases drop here: the texture returns to the pool.
                }
                PhotoResult::Failed { key, message } => {
                    if self.last_wanted == Some(key) {
                        self.photo_error = Some((key, message));
                    }
                }
            }
        }
    }

    /// The presenter may drop the current frame outside `offer` (subject change in `want`,
    /// device-loss `clear`): the egui registration must go with it, or egui could sample a
    /// texture the pool has handed to a new render.
    fn sync_photo_frame(&mut self) {
        let presented = self.presenter.current().map(|lease| lease.handle());
        if self
            .photo_frame
            .is_some_and(|(_, handle, _)| Some(handle) != presented)
        {
            if let Some((id, _, _)) = self.photo_frame.take() {
                self.renderer.free_texture(&id);
            }
        }
    }

    /// Snapshot + photo bookkeeping → the central panel's presentation state.
    fn photo_ui(&self, snapshot: &ApplicationSnapshot) -> crate::application::PhotoUi {
        let mut message = None;
        let mut loading = false;
        if let Some(image) = snapshot.editor.image.as_ref() {
            let document = &snapshot.editor.document;
            match &document.status {
                DocumentStatus::Ready => {
                    let params_missing = document.params.is_none();
                    if let Some(params) = &document.params {
                        if let Some(feature) = photo_worker::unsupported_reason(params) {
                            message = Some(format!(
                                "Darkroom can't render this photo yet: {feature} is not \
                                 supported by this build — nothing was changed."
                            ));
                        } else if let Some((key, error)) = &self.photo_error {
                            if self.last_wanted == Some(*key) {
                                message = Some(format!("{error} — {}", image.filename));
                            }
                        }
                    }
                    if message.is_none() {
                        let presented = self.presenter.current().map(|lease| lease.key());
                        let matches = self
                            .last_wanted
                            .as_ref()
                            .is_some_and(|wanted| Some(wanted) == presented);
                        loading = params_missing || !matches;
                    }
                }
                DocumentStatus::Loading => loading = true,
                DocumentStatus::Unsupported(reason) => {
                    message = Some(format!(
                        "Darkroom can't read this photo's saved adjustments ({reason}) — \
                     nothing was changed."
                    ))
                }
                DocumentStatus::Corrupt => {
                    message = Some(
                        "Darkroom can't read this photo's saved adjustments — the saved edit is \
                     not readable. Nothing was changed."
                            .to_string(),
                    )
                }
                DocumentStatus::Error(error) => {
                    message = Some(format!(
                        "Darkroom could not load this photo's adjustments: {error}. \
                     Nothing was changed."
                    ))
                }
                DocumentStatus::Empty => {}
            }
        }
        crate::application::PhotoUi {
            texture: self.photo_frame.as_ref().map(|(id, _, size)| (*id, *size)),
            message,
            loading,
            device_epoch: self.device_epoch,
            plan: None,
        }
    }

    /// Render the acquired frame; returns the delay until the next redraw.
    fn present(&mut self, frame: wgpu::SurfaceTexture, suboptimal: bool) -> Duration {
        let input = self.input.take_egui_input(&self.window);
        let output = if self.application.is_some() {
            self.drain_photo_results();
            let (snapshot, reopen) = {
                let application = self
                    .application
                    .as_mut()
                    .expect("checked by the branch above");
                (application.snapshot(), application.reopen_command())
            };
            let mut photo_view = self.photo_ui(&snapshot);
            let mut commands = Vec::new();
            let mut command_error = self.last_command_error.take();
            // Taken out and put back, like `command_error`: the closure needs `&mut` while
            // `self.egui` is borrowed.
            let mut settings_notices = std::mem::take(&mut self.settings.notices);
            let output = self.egui.run_ui(input, |root| {
                commands = crate::application::ui(
                    root,
                    &snapshot,
                    &reopen,
                    &mut command_error,
                    &mut photo_view,
                    &mut settings_notices,
                );
            });
            self.settings.notices = settings_notices;
            if let Some(plan) = photo_view.plan {
                if Some(plan.key) != self.last_wanted {
                    self.last_wanted = Some(plan.key);
                    self.photo_error = None;
                    self.presenter.want(plan.key);
                    self.photo.request(PhotoRequest {
                        key: plan.key,
                        path: plan.path,
                        content_hash: plan.content_hash,
                        params: plan.params,
                        view: plan.view,
                    });
                    // Repaint once the loading state is visible even with no other input.
                    self.window.request_redraw();
                }
            }
            self.sync_photo_frame();
            self.last_command_error = command_error;
            if !commands.is_empty() {
                self.window.request_redraw();
            }
            for command in commands {
                let application = self
                    .application
                    .as_mut()
                    .expect("checked by the branch above");
                match application.dispatch(command) {
                    Ok(()) => self.last_command_error = None,
                    Err(error) => {
                        self.last_command_error = Some((error.to_string(), Instant::now()));
                    }
                }
            }
            output
        } else {
            // The no-catalog qualification window has no command bar, but a settings problem is
            // just as visible here.
            let mut settings_notices = std::mem::take(&mut self.settings.notices);
            let output = self.egui.run_ui(input, |root| {
                crate::application::settings_notice_strip(root, &mut settings_notices);
                paint::ui(root);
            });
            self.settings.notices = settings_notices;
            output
        };
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |viewport| viewport.repaint_delay);
        self.input
            .handle_platform_output(&self.window, output.platform_output.clone());
        paint::paint(
            &self.gpu.context,
            &mut self.renderer,
            &frame.texture.create_view(&Default::default()),
            output,
            &self.egui,
            [self.size.width, self.size.height],
        );
        self.gpu.context.queue.present(frame);
        if suboptimal {
            self.resize(self.size);
        }
        delay
    }

    /// Record a terminal surface failure: stop acquiring, keep the event loop
    /// alive so the application can shut down through the normal path.
    fn fail(&mut self, failure: SurfaceFailure) {
        // Device/surface death: the presented lease's texture belongs to a dead context —
        // drop it and the egui registration together (the pool never sees these textures again).
        self.presenter.clear();
        self.sync_photo_frame();
        let streak = self.surface_state.streak();
        self.fatal = Some(match failure {
            SurfaceFailure::RetriesExhausted { last, attempts } => format!(
                "GPU surface acquisition failed {attempts} times in a row (last failure: {last}). \
                 Restart Darkroom; if this persists, check the display connection and GPU drivers."
            ),
            SurfaceFailure::Validation => "GPU surface validation error. \
                 Restart Darkroom; if this persists, check the display connection."
                .to_string(),
            SurfaceFailure::UncapturedGpu(message) => format!(
                "The GPU reported an error and Darkroom stopped rendering: {message}. \
                 Restart Darkroom; if this persists, check the display connection and GPU drivers."
            ),
            SurfaceFailure::RecreateFailed(message) => format!(
                "Failed to recreate the GPU surface after {streak} consecutive failures: \
                 {message}. Restart Darkroom; if this persists, check the display connection."
            ),
            SurfaceFailure::DeviceLost => format!(
                "GPU surface was lost {streak} times without ever presenting; this indicates a \
                 device-level GPU loss, which Darkroom cannot recover from at runtime. Restart \
                 Darkroom; if this persists, check the display connection and GPU drivers."
            )
            .to_string(),
        });
    }
}

impl App {
    fn schedule(&mut self, deadline: Option<Instant>) {
        if let Some(deadline) = deadline {
            self.next_repaint = Some(
                self.next_repaint
                    .map_or(deadline, |current| current.min(deadline)),
            );
        }
    }
}

impl ApplicationHandler<Option<Instant>> for App {
    fn user_event(&mut self, _: &ActiveEventLoop, deadline: Option<Instant>) {
        self.schedule(deadline);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.state {
            if let Some(application) = &mut state.application {
                let revision = application.snapshot().revision;
                if let Err(error) = application.pump() {
                    self.error = Some(error.to_string());
                    event_loop.exit();
                    return;
                }
                if application.is_shutdown_complete() {
                    event_loop.exit();
                    return;
                }
                if application.snapshot().revision != revision {
                    state.window.request_redraw();
                }
            }
        }
        if self
            .next_repaint
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.next_repaint = None;
            if let Some(state) = &self.state {
                state.window.request_redraw();
            }
        }
        event_loop.set_control_flow(
            self.next_repaint
                .map_or(ControlFlow::Wait, ControlFlow::WaitUntil),
        );
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none() {
            match State::new(event_loop, self.proxy.clone(), self.catalog_path.clone()) {
                Ok(state) => {
                    state.window.request_redraw();
                    self.state = Some(state);
                }
                Err(error) => {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut().filter(|state| state.window.id() == id) else {
            return;
        };
        let response = state.input.on_window_event(&state.window, &event);
        if response.repaint {
            state.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => {
                if let Some(application) = &mut state.application {
                    if let Err(error) = application.shutdown() {
                        self.error = Some(error.to_string());
                    }
                }
                if state.application.is_none() {
                    event_loop.exit();
                } else {
                    state.window.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                // A geometry change invalidates the previous failure mode.
                state.surface_state.on_visible();
                state.resize(size);
            }
            WindowEvent::Occluded(false) => {
                // Visibility returned; a prior failure streak no longer applies
                // and a suspended/occluded surface must be re-acquired now.
                state.surface_state.on_visible();
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let delay = state.redraw();
                if let Some(message) = state.fatal.clone() {
                    if !state.shutdown_requested {
                        state.shutdown_requested = true;
                        self.error = Some(message.clone());
                        if let Some(application) = &mut state.application {
                            // Flush catalog work through the normal shutdown path;
                            // `about_to_wait` exits via `is_shutdown_complete`.
                            if let Err(error) = application.shutdown() {
                                self.error = Some(format!("{message} (shutdown error: {error})"));
                            }
                            state.window.request_redraw();
                        } else {
                            event_loop.exit();
                        }
                    }
                }
                self.schedule(Instant::now().checked_add(delay));
            }
            _ => {}
        }
    }
}

fn egui_input(
    event_loop: &ActiveEventLoop,
    window: &Window,
    context: &core_pipeline::GpuContext,
    proxy: EventLoopProxy<Option<Instant>>,
) -> (egui::Context, egui_winit::State) {
    let egui = egui::Context::default();
    egui.set_request_repaint_callback(move |info| {
        if info.viewport_id == egui::ViewportId::ROOT {
            let _ = proxy.send_event(Instant::now().checked_add(info.delay));
        }
    });
    let input = egui_winit::State::new(
        egui.clone(),
        egui::ViewportId::ROOT,
        event_loop,
        Some(window.scale_factor() as f32),
        window.theme(),
        Some(context.max_texture_dim as usize),
    );
    (egui, input)
}
