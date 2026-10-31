use crate::{gpu::Gpu, paint};
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

enum Acquired {
    Frame(wgpu::SurfaceTexture, bool),
    Wait(Duration),
}

struct State {
    application: Option<crate::application::NativeApplication>,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    egui: egui::Context,
    input: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    size: winit::dpi::PhysicalSize<u32>,
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
        let application = catalog_path
            .map(|path| {
                crate::application::NativeApplication::new(
                    path,
                    true,
                    Arc::new(move || {
                        let _ = proxy.send_event(Some(Instant::now()));
                    }),
                )
            })
            .transpose()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            application,
            window,
            surface,
            config,
            gpu,
            egui,
            input,
            renderer,
            size,
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

    fn acquire(&mut self) -> Result<Acquired, String> {
        Ok(match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Acquired::Frame(frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Acquired::Frame(frame, true),
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.resize(self.size);
                Acquired::Wait(Duration::ZERO)
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.recover_surface()?;
                Acquired::Wait(Duration::ZERO)
            }
            wgpu::CurrentSurfaceTexture::Timeout => Acquired::Wait(Duration::from_millis(16)),
            wgpu::CurrentSurfaceTexture::Occluded => Acquired::Wait(Duration::MAX),
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("surface validation error".into())
            }
        })
    }

    fn redraw(&mut self) -> Result<Duration, String> {
        if self.size.width == 0 || self.size.height == 0 {
            return Ok(Duration::MAX);
        }
        let (frame, suboptimal) = match self.acquire()? {
            Acquired::Frame(frame, suboptimal) => (frame, suboptimal),
            Acquired::Wait(delay) => return Ok(delay),
        };
        let input = self.input.take_egui_input(&self.window);
        let output = if let Some(application) = &mut self.application {
            let snapshot = application.snapshot();
            let reopen = application.reopen_command();
            let mut commands = Vec::new();
            let output = self.egui.run_ui(input, |root| {
                commands = crate::application::ui(root, &snapshot, &reopen);
            });
            if !commands.is_empty() {
                self.window.request_redraw();
            }
            for command in commands {
                let _ = application.dispatch(command);
            }
            output
        } else {
            self.egui.run_ui(input, paint::ui)
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
        Ok(delay)
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
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::Occluded(false) => state.window.request_redraw(),
            WindowEvent::RedrawRequested => match state.redraw() {
                Ok(delay) => self.schedule(Instant::now().checked_add(delay)),
                Err(error) => {
                    self.error = Some(error);
                    event_loop.exit();
                }
            },
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
