//! No-GUI panorama harness: a directory of raws → LinearRaw DNG (+ sRGB JPEG proof).
//!
//! ```bash
//! cargo run --release -p core-raw --example stitch_cr3 -- <dir-of-raws> [out.dng]
//! ```
//!
//! The full app path (`src-tauri/src/panorama.rs`) does exactly this plus catalog registration;
//! this example is the fast feedback loop for stitch quality on real captures.

use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next()
            .expect("usage: stitch_cr3 <dir-of-raws> [out.dng]"),
    );
    let out = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("pano_out.dng"));

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| {
                    ["cr3", "cr2", "arw", "nef", "dng"].contains(&s.to_ascii_lowercase().as_str())
                })
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    anyhow::ensure!(
        paths.len() >= 2,
        "need at least 2 raws in {}",
        dir.display()
    );
    println!("{} source frames", paths.len());

    // Decode-on-demand source, mirroring `src-tauri/src/panorama.rs::CatalogFrameSource` so this
    // harness exercises the SAME streaming path the app uses: at most one full-res frame is
    // resident at a time (the eager `stitch(&[Frame])` path would hold all N — ~0.4 GB per 32 MP
    // frame — which is what this example used to do).
    struct DirFrameSource {
        paths: Vec<PathBuf>,
        metas: std::sync::Mutex<Vec<Option<core_raw::PanoColorMeta>>>,
    }
    impl core_pano::FrameSource for DirFrameSource {
        fn len(&self) -> usize {
            self.paths.len()
        }
        fn load(
            &self,
            i: usize,
            max_long_side: Option<u32>,
        ) -> Result<core_pano::LoadedFrame, core_pano::PanoError> {
            let p = &self.paths[i];
            let load = || -> anyhow::Result<core_raw::CameraNativeImage> {
                let src = core_raw::source_from_path(p)?;
                Ok(core_raw::develop_camera_native(&src)?)
            };
            let native =
                load().map_err(|e| core_pano::PanoError::Load(format!("{}: {e}", p.display())))?;
            let (full_width, full_height) = (native.width as usize, native.height as usize);
            if let Ok(mut m) = self.metas.lock() {
                m[i] = Some(native.meta.clone());
            }
            let img = core_raw::LinearImage {
                width: native.width,
                height: native.height,
                data: native.data,
            };
            let img = match max_long_side {
                Some(edge) => img.downscale_into_hq(edge),
                None => img,
            };
            Ok(core_pano::LoadedFrame {
                width: img.width as usize,
                height: img.height as usize,
                rgb: img.data,
                full_width,
                full_height,
                focal_seed_px: None,
            })
        }
    }

    let source = DirFrameSource {
        paths: paths.clone(),
        metas: std::sync::Mutex::new(vec![None; paths.len()]),
    };

    let t = Instant::now();
    let opt = core_pano::StitchOptions {
        projection: core_pano::Projection::Auto,
        boundary_warp: 0.0,
        auto_crop: true,
        max_long_side: 12_000,
        preview: false,
    };
    let result = core_pano::stitch_streaming(
        &source,
        &opt,
        &|phase| println!("  phase: {phase:?}"),
        &|| false,
    )?;
    let metas: Vec<Option<core_raw::PanoColorMeta>> = source.metas.into_inner().unwrap();
    println!(
        "stitch: {:.1}s → {}x{} ({:?}, capped={})",
        t.elapsed().as_secs_f32(),
        result.width,
        result.height,
        result.projection_used,
        result.capped
    );

    let t = Instant::now();
    let meta = metas[result.reference_index]
        .as_ref()
        .expect("reference frame meta captured during the low-res pass");
    core_raw::write_pano_dng(
        &out,
        result.width as u32,
        result.height as u32,
        &result.rgb,
        meta,
    )?;
    println!("DNG: {:.1}s → {}", t.elapsed().as_secs_f32(), out.display());

    // sRGB proof image next to the DNG for quick eyeballing.
    let jpeg = core_raw::native_to_srgb_jpeg(
        result.width as u32,
        result.height as u32,
        &result.rgb,
        meta,
        4096,
        90,
    )?;
    let jpg_path = out.with_extension("jpg");
    std::fs::write(&jpg_path, jpeg)?;
    println!("proof JPEG → {}", jpg_path.display());
    Ok(())
}
