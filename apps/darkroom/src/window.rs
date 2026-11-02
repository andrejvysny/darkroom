use crate::{
    chrome,
    gpu::Gpu,
    paint,
    photo_view::{EguiRegistry, PhotoView},
    photo_worker::{PhotoWorker, WorkerStatus},
    surface_state::{AcquireEvent, Action, Schedule, SurfaceFailure, SurfaceState},
    thumbs::ThumbStore,
};
use core_app::workflow::Command;
use core_model::identity::DeviceEpoch;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

/// Event-loop wakeups delivered through the proxy.
#[derive(Debug)]
enum UserEvent {
    /// Repaint no later than this instant (`None`: only re-run `about_to_wait`).
    Wake(Option<Instant>),
    /// The OS accessibility service asked for the tree, an action or deactivation.
    AccessKit(egui_winit::accesskit_winit::Event),
}

impl From<egui_winit::accesskit_winit::Event> for UserEvent {
    fn from(event: egui_winit::accesskit_winit::Event) -> Self {
        Self::AccessKit(event)
    }
}

pub fn run() -> Result<(), String> {
    run_with_catalog(None)
}

pub fn run_with_catalog(catalog_path: Option<std::path::PathBuf>) -> Result<(), String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
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
    let photo_error = app
        .state
        .as_mut()
        .and_then(|state| state.shutdown_photo().err());
    app.error.or(photo_error).map_or(Ok(()), Err)
}

struct App {
    catalog_path: Option<std::path::PathBuf>,
    state: Option<State>,
    error: Option<String>,
    next_repaint: Option<Instant>,
    proxy: EventLoopProxy<UserEvent>,
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
    /// Decode/develop worker on the shared device; `None` when it could not be started (the
    /// photo panel then says so) — never fatal for the window.
    photo: Option<PhotoWorker>,
    /// Presented frame, its egui registration and the failure state (see `photo_view.rs`).
    photo_view: PhotoView,
    /// Transient develop input state (drag in flight, dismissed banner).
    ui_state: ui_egui::UiState,
    /// Asynchronous thumbnail service; `None` without a catalog or when it could not start (the
    /// grid then shows placeholders).
    thumbs: Option<ThumbStore>,
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
        proxy: EventLoopProxy<UserEvent>,
        catalog_path: Option<std::path::PathBuf>,
    ) -> Result<Self, String> {
        let window = Arc::new(
            event_loop
                .create_window(chrome::window_attributes())
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
        // Shown only now: the AccessKit adapter `egui_input` attached refuses a visible window.
        window.set_visible(true);
        chrome::install_fonts(&egui);
        let renderer =
            egui_wgpu::Renderer::new(&gpu.context.device, config.format, Default::default());
        // Settings load before the catalog: preferences are the host's, not the catalog's, and a
        // failure there must never stop the window from opening.
        let mut settings = crate::settings::SettingsState::load(catalog_path.as_deref());
        let wake_workflow = proxy.clone();
        // Thumbnails live next to the catalog (`<dir>/catalog.db`, `<dir>/thumbs/`).
        let thumbs = catalog_path.as_deref().and_then(|path| {
            let wake_thumbs = proxy.clone();
            let root = path.parent()?.join("thumbs");
            match ThumbStore::new(
                egui.clone(),
                root,
                core_model::identity::CURRENT_PROCESS_VERSION.0,
                Arc::new(move || {
                    let _ = wake_thumbs.send_event(UserEvent::Wake(Some(Instant::now())));
                }),
            ) {
                Ok(store) => Some(store),
                Err(error) => {
                    eprintln!("thumbnails unavailable: {error}");
                    None
                }
            }
        });
        let application = catalog_path
            .map(|path| {
                crate::application::NativeApplication::new(
                    path,
                    true,
                    Arc::new(move || {
                        let _ = wake_workflow.send_event(UserEvent::Wake(Some(Instant::now())));
                    }),
                )
            })
            .transpose()
            .map_err(|e| e.to_string())?;
        // A mirror that could not start costs the session its sidecars, not the window: same
        // status-area notice path as the settings diagnostics, never a failed launch.
        if let Some(error) = application.as_ref().and_then(|a| a.mirror_error()) {
            settings.note(format!(
                "Darkroom could not start its sidecar mirror ({error}); your edits are saved in \
                 the catalog and reach the photo folders on the next launch."
            ));
        }
        let photo_proxy = proxy;
        let device_epoch = DeviceEpoch(1);
        let mut photo_view = PhotoView::new(device_epoch);
        let photo = match PhotoWorker::start(
            &gpu.context,
            Arc::clone(&gpu.pipeline),
            device_epoch,
            Arc::new(move || {
                let _ = photo_proxy.send_event(UserEvent::Wake(Some(Instant::now())));
            }),
        ) {
            Ok(worker) => Some(worker),
            Err(error) => {
                photo_view.set_unavailable(format!("Photo rendering is unavailable: {error}"));
                None
            }
        };
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
            photo_view,
            ui_state: ui_egui::UiState {
                chrome_inset_left: chrome::chrome_inset_left(),
                ..ui_egui::UiState::default()
            },
            thumbs,
        })
    }

    /// Run the import requests the last frame produced. Called between frames (never while a
    /// surface texture is held) because the folder picker is a blocking modal. A finished commit
    /// refreshes every library list.
    fn pump_imports(&mut self) {
        let Some(application) = self.application.as_mut() else {
            return;
        };
        let requests = std::mem::take(&mut self.ui_state.import_requests);
        let mut changed = !requests.is_empty();
        let platform = crate::import_platform::NativePlatform;
        for request in requests {
            if let Err(error) = application.handle_import(request, &platform) {
                self.last_command_error = Some((error.to_string(), Instant::now()));
            }
        }
        // The report itself is shown from the import view; the refresh covers failed runs too.
        let _ = application.take_import_finished();
        if application.take_import_refresh() {
            changed = true;
            for command in [
                Command::RefreshLibrary,
                Command::LoadDates,
                Command::LoadFolders,
                Command::LoadKeywords,
                Command::LoadCollections,
            ] {
                if let Err(error) = application.dispatch(command) {
                    self.last_command_error = Some((error.to_string(), Instant::now()));
                }
            }
        }
        if changed {
            self.window.request_redraw();
        }
    }

    fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        self.size = size;
        self.configure_surface();
        self.window.request_redraw();
    }

    /// (Re)configure the surface for `self.size`. Never requests a repaint: retries are paced by
    /// the surface state's retry instant, not by this call.
    fn configure_surface(&mut self) {
        if self.size.width == 0 || self.size.height == 0 {
            return;
        }
        self.config.width = self.size.width;
        self.config.height = self.size.height;
        self.surface
            .configure(&self.gpu.context.device, &self.config);
    }

    fn shutdown_photo(&mut self) -> Result<(), String> {
        let thumbs = self.thumbs.as_mut().map_or(Ok(()), ThumbStore::shutdown);
        let photo = self.photo.as_mut().map_or(Ok(()), PhotoWorker::shutdown);
        photo.and(thumbs)
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
        self.configure_surface();
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
        let now = Instant::now();
        if self.size.width == 0 || self.size.height == 0 {
            self.surface_state.on_event(AcquireEvent::ZeroSize, now);
            return Duration::MAX;
        }
        // Input/egui may request redraws at any rate; none of them may pull an acquisition
        // ahead of a scheduled retry instant.
        if let Err(remaining) = self.surface_state.may_acquire(now) {
            return remaining;
        }
        let (event, frame, suboptimal) = self.acquire();
        match self.surface_state.on_event(event, now) {
            Action::Present => self.present(
                frame.expect("Presented/Suboptimal always carry a frame"),
                suboptimal,
            ),
            Action::Reconfigure { after } => {
                self.configure_surface();
                after
            }
            Action::RecreateSurface { after } => match self.recover_surface() {
                Ok(()) => after,
                Err(message) => match self.surface_state.on_recreate_failed(message, now) {
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

    /// Render the acquired frame; returns the delay until the next redraw.
    fn present(&mut self, frame: wgpu::SurfaceTexture, suboptimal: bool) -> Duration {
        let input = self.input.take_egui_input(&self.window);
        crate::input_trace::egui_input(&input);
        let output = if let Some(application) = self.application.as_mut() {
            let (snapshot, reopen) = (application.snapshot(), application.reopen_command());
            self.photo_view.reconcile(&snapshot);
            let results = self
                .photo
                .as_mut()
                .map_or_else(Vec::new, PhotoWorker::drain_results);
            if let Some(WorkerStatus::Stopped(reason)) =
                self.photo.as_mut().map(PhotoWorker::status)
            {
                self.photo_view.set_unavailable(format!(
                    "Photo rendering stopped: {reason}. Restart Darkroom."
                ));
            }
            self.photo_view.accept_results(
                results,
                &mut EguiRegistry {
                    device: &self.gpu.context.device,
                    renderer: &mut self.renderer,
                },
            );
            let mut photo_ui = self.photo_view.ui_state(&snapshot);
            let histogram = self.photo_view.histogram(&snapshot);
            let mut no_thumbs = ui_egui::NoThumbs;
            let thumbs: &mut dyn ui_egui::ThumbSource = match self.thumbs.as_mut() {
                Some(store) => {
                    store.begin_frame(snapshot.catalog_epoch.0);
                    store
                }
                None => &mut no_thumbs,
            };
            let import_view = application.import_view();
            let mut commands = Vec::new();
            let mut command_error = self.last_command_error.take();
            // Taken out and put back, like `command_error`: the closure needs `&mut` while
            // `self.egui` is borrowed.
            let mut settings_notices = std::mem::take(&mut self.settings.notices);
            let output = self.egui.run_ui(input, |root| {
                commands = ui_egui::root(
                    root,
                    ui_egui::Frame {
                        snapshot: &snapshot,
                        reopen: &reopen,
                        command_error: &mut command_error,
                        photo: &mut photo_ui,
                        settings_notices: &mut settings_notices,
                        thumbs: &mut *thumbs,
                        histogram,
                        import: &import_view,
                    },
                    &mut self.ui_state,
                );
            });
            self.settings.notices = settings_notices;
            if let Some(store) = self.thumbs.as_mut() {
                store.end_frame();
            }
            let submitted = self.photo_view.submit(photo_ui.plan.take(), photo_ui.retry);
            if let (Some(request), Some(worker)) = (submitted, self.photo.as_ref()) {
                worker.request(request);
                // Repaint once the loading state is visible even with no other input.
                self.window.request_redraw();
            }
            self.last_command_error = command_error;
            if !commands.is_empty() || !self.ui_state.import_requests.is_empty() {
                self.window.request_redraw();
            }
            for command in commands {
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
                ui_egui::settings_notice_strip(root, &mut settings_notices);
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
        self.photo_view.after_submit(&mut EguiRegistry {
            device: &self.gpu.context.device,
            renderer: &mut self.renderer,
        });
        self.gpu.context.queue.present(frame);
        if suboptimal {
            self.configure_surface();
        }
        delay
    }

    /// Record a terminal surface failure: stop acquiring, keep the event loop
    /// alive so the application can shut down through the normal path.
    fn fail(&mut self, failure: SurfaceFailure) {
        // Device/surface death: the presented lease's texture belongs to a dead context —
        // drop it and the egui registration together (the pool never sees these textures again).
        self.photo_view.retire_for_device_loss(&mut EguiRegistry {
            device: &self.gpu.context.device,
            renderer: &mut self.renderer,
        });
        self.fatal = Some(failure.message(self.surface_state.streak()));
    }
}

impl State {
    /// Route an OS accessibility request into egui. The tree is only built while an assistive
    /// client (VoiceOver, Accessibility Inspector, automation) is attached.
    fn on_accesskit(&mut self, event: egui_winit::accesskit_winit::Event) {
        use egui_winit::accesskit_winit::WindowEvent as Ax;
        if event.window_id != self.window.id() {
            return;
        }
        match event.window_event {
            Ax::InitialTreeRequested => self.egui.enable_accesskit(),
            Ax::ActionRequested(request) => self.input.on_accesskit_action_request(request),
            Ax::AccessibilityDeactivated => self.egui.disable_accesskit(),
        }
        self.window.request_redraw();
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

impl ApplicationHandler<UserEvent> for App {
    fn user_event(&mut self, _: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake(deadline) => self.schedule(deadline),
            UserEvent::AccessKit(event) => {
                if let Some(state) = self.state.as_mut() {
                    state.on_accesskit(event);
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.state {
            state.pump_imports();
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
                // Autosave debounce / retry backoff / barrier timeout fire with no input.
                let deadline = application.next_deadline();
                self.schedule(deadline);
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
        crate::input_trace::winit_event(&event);
        let response = state.input.on_window_event(&state.window, &event);
        if response.repaint {
            state.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => {
                if let Some(application) = &mut state.application {
                    // A refused or later-failing quit keeps the window open: the error is
                    // shown in the UI (command error / save banner) and a second close retries.
                    state.last_command_error = application
                        .shutdown()
                        .err()
                        .map(|error| (error.to_string(), Instant::now()));
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
            WindowEvent::Focused(true) => {
                if let Some(store) = state.thumbs.as_ref() {
                    store.invalidate_presence();
                }
                state.window.request_redraw();
            }
            WindowEvent::Occluded(false) => {
                // Visibility returned; a prior failure streak no longer applies
                // and a suspended/occluded surface must be re-acquired now.
                state.surface_state.on_visible();
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let delay =
                    match state
                        .surface_state
                        .schedule(Instant::now(), self.next_repaint, true)
                    {
                        Schedule::AcquireNow => state.redraw(),
                        Schedule::WaitUntil(at) => at.saturating_duration_since(Instant::now()),
                        Schedule::Idle => Duration::MAX,
                    };
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
    proxy: EventLoopProxy<UserEvent>,
) -> (egui::Context, egui_winit::State) {
    let accesskit_proxy = proxy.clone();
    let egui = egui::Context::default();
    egui.set_request_repaint_callback(move |info| {
        if info.viewport_id == egui::ViewportId::ROOT {
            let _ = proxy.send_event(UserEvent::Wake(Instant::now().checked_add(info.delay)));
        }
    });
    let mut input = egui_winit::State::new(
        egui.clone(),
        egui::ViewportId::ROOT,
        event_loop,
        Some(window.scale_factor() as f32),
        window.theme(),
        Some(context.max_texture_dim as usize),
    );
    input.init_accesskit(event_loop, window, accesskit_proxy);
    (egui, input)
}
