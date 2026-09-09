//! **Test support**: author small, fully specified DNG files (Bayer CFA or monochrome).
//!
//! Not part of the product surface — nothing in the app calls this. It exists because every other
//! RAW in the tree is a 32 MP Canon CR3 that only the dev machine has: the sensor properties this
//! crate's guards care about (a non-2×2 CFA, a `BlackIsZero` sensor, a clipped highlight, a
//! specific EXIF orientation) cannot be produced from it. These writers make a few-kilobyte file
//! with exactly the property under test, so the checks run in CI with no fixture.
//!
//! The output goes through rawler's own `DngWriter` and is read back by rawler's own DNG decoder,
//! so a fixture that round-trips proves the *real* decode path, not a mock of it.

use std::collections::HashMap;
use std::io::BufWriter;
use std::path::Path;

use rawler::decoders::Camera;
use rawler::dng::writer::DngWriter;
use rawler::dng::{CropMode, DngCompression, DngPhotometricConversion, DNG_VERSION_V1_4};
use rawler::imgop::xyz::{FlatColorMatrix, Illuminant, XYZ_TO_SRGB_D65};
use rawler::pixarray::PixU16;
use rawler::rawimage::{BlackLevel, CFAConfig, RawPhotometricInterpretation, WhiteLevel};
use rawler::tags::{ExifTag, TiffCommonTag};
use rawler::{cfa::PlaneColor, RawImage, CFA};

use crate::error::RawError;

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(e.to_string())
}

/// A rectangle in sensor pixels: `(x, y, width, height)`.
pub type Block = (u32, u32, u32, u32);

/// Everything the synthetic writers need. [`Default`] is a healthy 2×2 RGGB sensor with a
/// mid-grey frame, so a test only overrides the one property it is about.
#[derive(Debug, Clone)]
pub struct SynthSpec {
    /// Sensor width in pixels. Must be a positive multiple of the CFA tile width.
    pub width: u32,
    /// Sensor height in pixels. Must be a positive multiple of the CFA tile height.
    pub height: u32,
    /// CFA pattern string in rawler's notation — `"RGGB"` (2×2) or a 36-char X-Trans pattern.
    pub cfa: &'static str,
    /// Black level, in raw counts, for every CFA position.
    pub black: u16,
    /// White (saturation) level, in raw counts.
    pub white: u16,
    /// As-shot white balance `[r, g, b, e]`, written as `AsShotNeutral` (the reciprocals).
    pub wb: [f32; 4],
    /// XYZ→camera matrix, padded to four rows like rawler's own. Written as `ColorMatrix1` with a
    /// D65 calibration illuminant. The default is XYZ→sRGB(D65), i.e. a notional sensor whose
    /// primaries are sRGB's — the same choice `pano.rs`'s tests make, so a neutral raw develops
    /// neutral and the numbers are hand-checkable.
    pub xyz_to_cam: [[f32; 3]; 4],
    /// EXIF orientation tag (1–8) written into the root IFD.
    pub orientation: u16,
    /// Region forced to `white` — a blown highlight. Applied last, so it wins over `neutral_block`.
    pub saturated_block: Option<Block>,
    /// Region filled so it develops NEUTRAL: the per-CFA-position values are pre-divided by the WB
    /// gains, so after white balance all three channels land on the same value (see
    /// [`write_synthetic_bayer_dng`]). Ignored by the monochrome writer, which has no WB.
    pub neutral_block: Option<Block>,
    /// Raw level the rest of the frame is filled with (a flat mid-grey mosaic).
    pub base_level: u16,
}

impl Default for SynthSpec {
    fn default() -> Self {
        let mut xyz_to_cam = [[0f32; 3]; 4];
        for (row, src) in xyz_to_cam.iter_mut().zip(XYZ_TO_SRGB_D65.iter()) {
            *row = *src;
        }
        Self {
            width: 64,
            height: 48,
            cfa: "RGGB",
            black: 512,
            white: 16383,
            wb: [2.0, 1.0, 1.5, f32::NAN],
            xyz_to_cam,
            orientation: 1,
            saturated_block: None,
            neutral_block: None,
            // 8192 - 512 = 7680 is exactly divisible by the default gains (7680/2 and 7680*2/3),
            // so the neutral patch is quantization-free and the test tolerance stays meaningful.
            base_level: 8192,
        }
    }
}

impl SynthSpec {
    fn validate(&self, cfa: &CFA) -> Result<(), RawError> {
        if self.width == 0 || self.height == 0 {
            return Err(de("synthetic sensor must have a non-zero size"));
        }
        if !cfa.is_valid() {
            return Err(de(format!("unknown CFA pattern {:?}", self.cfa)));
        }
        if !(self.width as usize).is_multiple_of(cfa.width)
            || !(self.height as usize).is_multiple_of(cfa.height)
        {
            return Err(de(format!(
                "{}x{} is not a whole number of {}x{} CFA tiles",
                self.width, self.height, cfa.width, cfa.height
            )));
        }
        if self.black >= self.white {
            return Err(de(format!(
                "black level {} must be below white level {}",
                self.black, self.white
            )));
        }
        Ok(())
    }

    /// `(x0, y0, x1, y1)` of a block, clamped to the sensor. `None` for an empty/absent block.
    fn clamped(&self, block: Option<Block>) -> Option<(u32, u32, u32, u32)> {
        let (x, y, w, h) = block?;
        let x1 = x.saturating_add(w).min(self.width);
        let y1 = y.saturating_add(h).min(self.height);
        (x < x1 && y < y1).then_some((x, y, x1, y1))
    }

    fn camera(&self, name: &str) -> Camera {
        let mut cam = Camera::new();
        cam.make = "Darkroom".into();
        cam.model = name.into();
        cam.clean_make = cam.make.clone();
        cam.clean_model = cam.model.clone();
        let flat: FlatColorMatrix = self.xyz_to_cam[0..3].iter().flatten().copied().collect();
        cam.color_matrix = HashMap::from([(Illuminant::D65, flat)]);
        cam
    }
}

/// Write an uncompressed 16-bit **CFA** DNG matching `spec`.
///
/// The mosaic is `base_level` everywhere, then `neutral_block`, then `saturated_block`.
///
/// The neutral block is the interesting one. A patch that is *numerically* flat in the mosaic is
/// NOT neutral after develop — white balance multiplies each CFA colour by a different gain. To get
/// a patch that develops to R=G=B, the raw values must be pre-divided by those gains:
/// `value_c = black + (base - black) * g_gain / gain_c`. That makes "did white balance and the
/// colour matrix stay wired together correctly" a one-line assertion on chroma.
pub fn write_synthetic_bayer_dng(dest: &Path, spec: &SynthSpec) -> Result<(), RawError> {
    let cfa = CFA::new(spec.cfa);
    spec.validate(&cfa)?;

    let (w, h) = (spec.width as usize, spec.height as usize);
    let neutral = spec.clamped(spec.neutral_block);
    let saturated = spec.clamped(spec.saturated_block);
    let span = (spec.base_level as f32) - (spec.black as f32);
    let g_gain = spec.wb[1];

    let mut data = vec![spec.base_level; w * h];
    for y in 0..h {
        for x in 0..w {
            let (xu, yu) = (x as u32, y as u32);
            if let Some((x0, y0, x1, y1)) = neutral {
                if (x0..x1).contains(&xu) && (y0..y1).contains(&yu) {
                    let plane = cfa.color_at(y, x);
                    let gain = spec.wb.get(plane).copied().unwrap_or(g_gain);
                    let scale = if gain.is_finite() && gain > 0.0 && g_gain.is_finite() {
                        g_gain / gain
                    } else {
                        1.0
                    };
                    let v = (spec.black as f32 + span * scale).round();
                    data[y * w + x] = v.clamp(0.0, spec.white as f32) as u16;
                }
            }
            if let Some((x0, y0, x1, y1)) = saturated {
                if (x0..x1).contains(&xu) && (y0..y1).contains(&yu) {
                    data[y * w + x] = spec.white;
                }
            }
        }
    }

    let mut cam = spec.camera("SynthBayer");
    cam.cfa = cfa.clone();
    cam.plane_color = PlaneColor::new("RGB");
    let rawimage = RawImage::new(
        cam,
        PixU16::new_with(data, w, h),
        1,
        spec.wb,
        RawPhotometricInterpretation::Cfa(CFAConfig::new(&cfa, &PlaneColor::new("RGB"))),
        Some(BlackLevel::new(&[spec.black as u32; 4], 2, 2, 1)),
        Some(WhiteLevel::new(vec![spec.white as u32])),
        false,
    );
    write_dng(dest, &rawimage, spec.orientation, None)
}

/// Write an uncompressed 16-bit **monochrome** (`PhotometricInterpretation = BlackIsZero`) DNG.
///
/// `neutral_block` is ignored (there is no white balance on a mono sensor); `saturated_block` still
/// applies.
///
/// A `CFAPattern` tag is written even though the file has no colour filter array. That is not
/// decoration: rawler's DNG decoder reads the CFA for every photometric except `LinearRaw`
/// (`decoders/dng.rs::make_camera`), so a `BlackIsZero` DNG without that tag fails to open at all
/// and the monochrome develop path could never be exercised. The tag is inert on read — the
/// photometric still decodes as `BlackIsZero`, which is what `RawImage::is_monochrome()` keys off.
pub fn write_synthetic_mono_dng(dest: &Path, spec: &SynthSpec) -> Result<(), RawError> {
    let cfa = CFA::new("RGGB"); // shape only — see the doc comment above
    spec.validate(&cfa)?;

    let (w, h) = (spec.width as usize, spec.height as usize);
    let mut data = vec![spec.base_level; w * h];
    if let Some((x0, y0, x1, y1)) = spec.clamped(spec.saturated_block) {
        for y in y0..y1 {
            for x in x0..x1 {
                data[y as usize * w + x as usize] = spec.white;
            }
        }
    }

    let mut cam = spec.camera("SynthMono");
    cam.cfa = cfa.clone();
    let rawimage = RawImage::new(
        cam,
        PixU16::new_with(data, w, h),
        1,
        [f32::NAN; 4], // a mono sensor reports no as-shot white balance
        RawPhotometricInterpretation::BlackIsZero,
        Some(BlackLevel::new(&[spec.black as u32], 1, 1, 1)),
        Some(WhiteLevel::new(vec![spec.white as u32])),
        false,
    );
    write_dng(dest, &rawimage, spec.orientation, Some(cfa.flat_pattern()))
}

/// Shared tail: one uncompressed raw subframe, base tags, EXIF orientation. No embedded preview or
/// thumbnail — nothing here reads one, and leaving them out keeps the root IFD free of the
/// `NewSubFileType`/`Compression` pair that `get_raw_ifd` has to skip.
fn write_dng(
    dest: &Path,
    rawimage: &RawImage,
    orientation: u16,
    extra_cfa_pattern: Option<Vec<u8>>,
) -> Result<(), RawError> {
    let file = std::fs::File::create(dest)?;
    let mut dng = DngWriter::new(BufWriter::new(file), DNG_VERSION_V1_4).map_err(de)?;

    let mut frame = dng.subframe(0);
    frame
        .raw_image(
            rawimage,
            CropMode::None,
            DngCompression::Uncompressed,
            DngPhotometricConversion::Original,
            1,
        )
        .map_err(de)?;
    if let Some(pattern) = extra_cfa_pattern {
        frame
            .ifd_mut()
            .add_tag(TiffCommonTag::CFAPattern, &pattern[..]);
    }
    frame.finalize().map_err(de)?;

    dng.load_base_tags(rawimage).map_err(de)?;
    // Written last: `add_tag` replaces, and this is the tag `read_metadata` (and therefore
    // `LinearImage::oriented`) reads back.
    dng.root_ifd_mut()
        .add_tag(ExifTag::Orientation, orientation);
    dng.root_ifd_mut()
        .add_tag(TiffCommonTag::Software, "Darkroom synth");
    dng.close().map_err(de)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawler::decoders::RawDecodeParams;
    use rawler::rawsource::RawSource;

    fn temp_dng(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "darkroom-synthcheck-{}-{}.dng",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn decode(path: &Path) -> RawImage {
        let src = RawSource::new(path).expect("open synthetic DNG");
        rawler::get_decoder(&src)
            .expect("decoder for DNG")
            .raw_image(&src, &RawDecodeParams::default(), false)
            .expect("decode synthetic DNG")
    }

    /// The rawler-level half of the round-trip (the public-API half lives in
    /// `tests/synthetic_bayer.rs`, which must not name rawler types): a written CFA DNG comes back
    /// as a `Cfa` photometric with cpp=1 and the levels/WB it was given.
    #[test]
    fn bayer_dng_round_trips_as_cfa() {
        let spec = SynthSpec::default();
        let path = temp_dng("cfa");
        write_synthetic_bayer_dng(&path, &spec).expect("write");
        let raw = decode(&path);

        match &raw.photometric {
            RawPhotometricInterpretation::Cfa(c) => {
                assert_eq!(c.cfa.to_string(), spec.cfa);
                assert_eq!((c.cfa.width, c.cfa.height), (2, 2));
            }
            other => panic!("photometric must survive as Cfa, got {other:?}"),
        }
        assert_eq!(raw.cpp, 1);
        assert_eq!(
            (raw.width, raw.height),
            (spec.width as usize, spec.height as usize)
        );
        assert_eq!(raw.blacklevel.as_bayer_array()[0], spec.black as f32);
        assert_eq!(raw.whitelevel.as_bayer_array()[0], spec.white as f32);
        for i in 0..3 {
            assert!((raw.wb_coeffs[i] - spec.wb[i]).abs() < 1e-3, "{i}");
        }
        assert!(!raw.is_monochrome());
        let _ = std::fs::remove_file(&path);
    }

    /// A written monochrome DNG comes back as `BlackIsZero` — the photometric
    /// `RawImage::is_monochrome()` keys off, and the one rawler's `apply_scaling()` cannot handle.
    #[test]
    fn mono_dng_round_trips_as_black_is_zero() {
        let spec = SynthSpec::default();
        let path = temp_dng("mono");
        write_synthetic_mono_dng(&path, &spec).expect("write");
        let raw = decode(&path);

        assert!(
            matches!(raw.photometric, RawPhotometricInterpretation::BlackIsZero),
            "got {:?}",
            raw.photometric
        );
        assert!(raw.is_monochrome());
        assert_eq!(raw.cpp, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_specs_are_refused() {
        let path = temp_dng("invalid");
        for spec in [
            SynthSpec {
                width: 0,
                ..Default::default()
            },
            // 63 is not a whole number of 2-wide CFA tiles.
            SynthSpec {
                width: 63,
                ..Default::default()
            },
            SynthSpec {
                black: 16383,
                white: 512,
                ..Default::default()
            },
        ] {
            assert!(write_synthetic_bayer_dng(&path, &spec).is_err());
        }
        let _ = std::fs::remove_file(&path);
    }
}
