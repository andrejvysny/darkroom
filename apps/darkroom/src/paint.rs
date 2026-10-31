use core_pipeline::GpuContext;
use egui_wgpu::{Renderer, ScreenDescriptor};

pub fn paint(
    context: &GpuContext,
    renderer: &mut Renderer,
    view: &wgpu::TextureView,
    mut output: egui::FullOutput,
    egui: &egui::Context,
    size: [u32; 2],
) {
    let screen = ScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: output.pixels_per_point,
    };
    let jobs = egui.tessellate(output.shapes, output.pixels_per_point);
    for (id, delta) in &output.textures_delta.set {
        for update in delta {
            renderer.update_texture(&context.device, &context.queue, *id, update);
        }
    }
    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("native-egui"),
        });
    let callbacks = renderer.update_buffers(
        &context.device,
        &context.queue,
        &mut encoder,
        &jobs,
        &screen,
    );
    draw(&mut encoder, renderer, view, &jobs, &screen);
    context.queue.submit(
        callbacks
            .into_iter()
            .chain(std::iter::once(encoder.finish())),
    );
    for id in &output.textures_delta.free {
        renderer.free_texture(id);
    }
    output.textures_delta.clear();
}

pub fn ui(root: &mut egui::Ui) {
    egui::CentralPanel::default().show(root, |ui| {
        ui.heading("Darkroom native stack qualification");
        ui.label("Foundation probe: winit, egui and the shared GPU develop pipeline.");
        ui.label("This window does not provide the photo library or editor.");
    });
}

fn draw(
    encoder: &mut wgpu::CommandEncoder,
    renderer: &Renderer,
    view: &wgpu::TextureView,
    jobs: &[egui::ClippedPrimitive],
    screen: &ScreenDescriptor,
) {
    let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("native-egui"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    renderer.render(&mut pass.forget_lifetime(), jobs, screen);
}
