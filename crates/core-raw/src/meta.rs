//! Metadata extraction (fast — no pixel decode) and capture-fingerprint canonicalization.

use crate::error::RawError;
use chrono::NaiveDateTime;
use rawler::decoders::{RawDecodeParams, RawMetadata};
use rawler::rawsource::RawSource;
use serde::{Deserialize, Serialize};

/// Catalog-facing metadata for one RAW file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawMeta {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub body_serial: Option<String>,
    pub lens: Option<String>,
    /// Epoch seconds parsed from EXIF `DateTimeOriginal` (naive time treated as UTC).
    pub capture_date: Option<i64>,
    /// Raw EXIF `DateTimeOriginal` string ("YYYY:MM:DD HH:MM:SS") for date routing + fingerprint.
    pub date_time_original: Option<String>,
    pub sub_sec: Option<String>,
    pub iso: Option<i64>,
    pub shutter: Option<String>,
    pub aperture: Option<f64>,
    pub focal_length: Option<f64>,
    pub orientation: Option<i64>,
}

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(e.to_string())
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Parse "YYYY:MM:DD HH:MM:SS" → epoch seconds (naive interpreted as UTC).
fn parse_exif_dt(s: &str) -> Option<i64> {
    NaiveDateTime::parse_from_str(s.trim(), "%Y:%m:%d %H:%M:%S")
        .ok()
        .map(|dt| dt.and_utc().timestamp())
}

impl RawMeta {
    pub fn from_metadata(md: &RawMetadata) -> Self {
        let exif = &md.exif;
        let dto = exif
            .date_time_original
            .clone()
            .or_else(|| exif.create_date.clone());
        let lens = md
            .lens
            .as_ref()
            .and_then(|l| non_empty(&l.lens_name))
            .or_else(|| exif.lens_model.as_deref().and_then(non_empty));

        RawMeta {
            camera_make: non_empty(&md.make),
            camera_model: non_empty(&md.model),
            body_serial: exif.serial_number.as_deref().and_then(non_empty),
            lens,
            capture_date: dto.as_deref().and_then(parse_exif_dt),
            date_time_original: dto,
            sub_sec: exif.sub_sec_time_original.as_deref().and_then(non_empty),
            iso: exif
                .iso_speed_ratings
                .map(|v| v as i64)
                .or(exif.iso_speed.map(|v| v as i64)),
            shutter: exif.exposure_time.as_ref().map(|r| {
                let v = r.as_f32();
                if v >= 1.0 {
                    format!("{}s", (v * 10.0).round() / 10.0)
                } else if r.n != 0 {
                    format!("1/{}", (r.d as f32 / r.n as f32).round() as u32)
                } else {
                    "0".to_string()
                }
            }),
            aperture: exif.fnumber.as_ref().map(|r| r.as_f32() as f64),
            focal_length: exif.focal_length.as_ref().map(|r| r.as_f32() as f64),
            orientation: exif.orientation.map(|v| v as i64),
        }
    }
}

/// Read metadata from an open [`RawSource`] WITHOUT decoding pixels (fast indexing path).
pub fn read_metadata(src: &RawSource) -> Result<RawMeta, RawError> {
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let md = decoder
        .raw_metadata(src, &RawDecodeParams::default())
        .map_err(de)?;
    Ok(RawMeta::from_metadata(&md))
}

/// EXIF "YYYY:MM:DD HH:MM:SS" → ISO-8601 "YYYY-MM-DDTHH:MM:SS" (for the fingerprint canonical form).
fn to_iso8601(s: &str) -> String {
    NaiveDateTime::parse_from_str(s.trim(), "%Y:%m:%d %H:%M:%S")
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Capture fingerprint (spec Appendix A): BLAKE3 of the canonical string
/// `camera_model | body_serial | DateTimeOriginal(ISO-8601) | SubSecTime | shutter_count | width | height`.
///
/// Returns `None` when too few discriminating fields are present (model + date) — such a
/// fingerprint is low-confidence and excluded from auto-grouping.
pub fn capture_fingerprint(meta: &RawMeta, width: u32, height: u32) -> Option<[u8; 32]> {
    let model = meta.camera_model.as_deref().unwrap_or("").to_lowercase();
    let dto_iso = meta
        .date_time_original
        .as_deref()
        .map(to_iso8601)
        .unwrap_or_default();

    // Low-confidence guard: require the two most discriminating fields.
    if model.trim().is_empty() || dto_iso.is_empty() {
        return None;
    }

    let serial = meta.body_serial.as_deref().unwrap_or("").to_lowercase();
    let subsec = meta.sub_sec.as_deref().unwrap_or("");
    let shutter_count = ""; // not available from standard EXIF on Canon
    let canonical = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        model.trim(),
        serial.trim(),
        dto_iso,
        subsec.trim(),
        shutter_count,
        width,
        height
    );
    Some(*blake3::hash(canonical.as_bytes()).as_bytes())
}
