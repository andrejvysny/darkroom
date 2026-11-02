mod application;
mod chrome;
mod gpu;
mod import_platform;
mod native_app;
mod paint;
mod photo_view;
mod photo_worker;
mod present;
mod settings;
mod surface_state;
mod thumbs;
mod window;

/// Exit-code table for `--catalog-probe` (the JSON body on stdout carries `kind` + `error`):
///
/// | code | meaning                                                        |
/// |------|----------------------------------------------------------------|
/// | 0    | success (`status: "ok"`)                                       |
/// | 1    | usage error / other failures                                   |
/// | 2    | open failed — kinds `corrupt`, `schema_too_new`, `not_open`, `catalog` |
/// | 3    | selection failed (`selection`)                                 |
/// | 4    | runtime shutdown did not complete (`shutdown`)                          |
/// | 5    | catalog read failed after a successful open (`query`)                    |
///
/// `--photo-probe` adds:
///
/// | code | meaning                                                        |
/// |------|----------------------------------------------------------------|
/// | 6    | render failure — decode/GPU/readback/egui-present, native ≠ offscreen, or a
/// |      | presented frame whose aspect does not match the cropped frame aspect         |
/// | 7    | document not renderable (not `Ready`; denoise/AI-mask edits)   |
///
/// `--import-settings FILE` (optionally with `--catalog PATH` to name the settings directory)
/// merges a legacy exported settings file into the native settings file and prints the
/// `ImportReport` as JSON: exit 0 on `status: "ok"`, exit 1 with `{"status":"error",...}` when the
/// file is missing/unparseable or the settings file cannot be opened (e.g. written by a newer
/// build). The catalog is never opened.
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let code = match args.as_slice() {
        [] => match window::run() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        [arg] if arg == "--gpu-probe" => match probe() {
            Ok(()) => 0,
            Err(error) => {
                println!("{}", serde_json::json!({"status":"error", "error": error}));
                1
            }
        },
        [arg, path] if arg == "--catalog" => match window::run_with_catalog(Some(path.into())) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        [arg, path] if arg == "--catalog-probe" => catalog_probe(path, true),
        [arg, path, opt] if arg == "--catalog-probe" && opt == "--no-select" => {
            catalog_probe(path, false)
        }
        [arg, path] if arg == "--photo-probe" => photo_probe(path, None),
        [arg, path, opt, value] if arg == "--photo-probe" && opt == "--image" => {
            match value.parse::<i64>() {
                Ok(id) => photo_probe(path, Some(core_model::identity::ImageId(id))),
                Err(_) => {
                    println!(
                        "{}",
                        serde_json::json!({"status":"error", "kind":"usage", "error": format!("invalid image id {value:?}")})
                    );
                    1
                }
            }
        }
        [arg, file] if arg == "--import-settings" => import_settings(None, file),
        [flag, path, arg, file] if flag == "--catalog" && arg == "--import-settings" => {
            import_settings(Some(path), file)
        }
        [arg, file, flag, path] if arg == "--import-settings" && flag == "--catalog" => {
            import_settings(Some(path), file)
        }
        _ => {
            eprintln!(
                "usage: darkroom-native [--gpu-probe | --catalog PATH | --catalog-probe PATH [--no-select] | --photo-probe PATH [--image ID] | --import-settings FILE [--catalog PATH]]"
            );
            1
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
}

fn catalog_probe(path: &str, select_first: bool) -> i32 {
    match application::probe(path.into(), select_first) {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"status":"error", "kind": error.kind(), "error": error.message()})
            );
            error.exit_code()
        }
    }
}

/// `--import-settings FILE`: import a legacy exported settings file into the settings directory
/// (the `--catalog PATH` parent when given, else the app-data directory) and print the
/// `ImportReport` as JSON on stdout. Never opens the catalog, never touches browser storage.
fn import_settings(catalog: Option<&str>, file: &str) -> i32 {
    let report = settings::import_settings_file(
        catalog.map(std::path::Path::new),
        std::path::Path::new(file),
    );
    match report {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"status":"error", "kind":"settings", "error": error})
            );
            1
        }
    }
}

fn photo_probe(path: &str, image_id: Option<core_model::identity::ImageId>) -> i32 {
    match application::photo_probe(path.into(), image_id) {
        Ok(value) => {
            println!("{value}");
            // Exit 0 only on byte-exactness; a mismatch is a render failure (6).
            i32::from(value.get("identical") != Some(&serde_json::Value::Bool(true)))
        }
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"status":"error", "kind": error.kind(), "error": error.message()})
            );
            error.exit_code()
        }
    }
}

fn probe() -> Result<(), String> {
    let gpu = gpu::Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle()),
        None,
    )?;
    let bytes = gpu.develop_probe()?;
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = paint::probe_target(&gpu.context.device, format, [256, 128]);
    let mut renderer = egui_wgpu::Renderer::new(&gpu.context.device, format, Default::default());
    let egui = egui::Context::default();
    egui.set_missing_glyph_policy(egui::epaint::text::MissingGlyphPolicy::Panic);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(256.0, 128.0),
        )),
        ..Default::default()
    };
    let output = egui.run_ui(input, paint::ui);
    paint::paint(
        &gpu.context,
        &mut renderer,
        &texture.create_view(&Default::default()),
        output,
        &egui,
        [256, 128],
    );
    gpu.context
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::json!({"status":"ok", "adapter":gpu.context.info, "develop_bytes":bytes, "egui_rendered":true})
    );
    Ok(())
}
