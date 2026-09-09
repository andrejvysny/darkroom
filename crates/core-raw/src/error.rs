//! Error taxonomy for RAW decode.
//!
//! The catalog needs to tell three very different failures apart: a file this build simply cannot
//! decode ([`RawError::Unsupported`] — retrying is pointless until rawler grows the format), a file
//! that IS decodable but arrived damaged ([`RawError::Decode`] — worth re-reading from the card),
//! and a decoder that blew up mid-decode ([`RawError::DecoderPanic`] — contained by
//! [`crate::panic`], the file is skipped and the app survives). [`FailureKind`] is that
//! three-way split in a form the UI can branch on.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RawError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(String),
    #[error("no embedded preview/thumbnail found")]
    NoPreview,
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
    /// The file is well-formed but this build cannot develop it (unknown body, unsupported
    /// compression, non-Bayer CFA). Carries rawler's own camera identification so the UI can name
    /// the body without a second metadata read.
    #[error("unsupported: {detail} (make '{make}', model '{model}', mode '{mode}')")]
    Unsupported {
        make: String,
        model: String,
        mode: String,
        detail: String,
    },
    /// A panic inside a decoder, caught by [`crate::panic::catch_decode_panic`]. Distinct from
    /// `Decode` because it means rawler hit a `todo!()`/`unimplemented!()`/index bug, not that the
    /// bytes are damaged.
    #[error("decoder panic: {0}")]
    DecoderPanic(String),
}

impl From<rawler::RawlerError> for RawError {
    fn from(e: rawler::RawlerError) -> Self {
        match e {
            rawler::RawlerError::Unsupported {
                what,
                model,
                make,
                mode,
            } => RawError::Unsupported {
                make,
                model,
                mode,
                detail: what,
            },
            // rawler files several "we don't do this format" cases under `DecoderFailed` (e.g.
            // "NEF compression HighEfficency is not supported"). Those are permanent for this
            // build, not damaged bytes, so promote them — otherwise the UI would tell the user to
            // re-copy a perfectly good Z 9 file.
            rawler::RawlerError::DecoderFailed(msg) if is_unsupported_message(&msg) => {
                RawError::Unsupported {
                    make: String::new(),
                    model: String::new(),
                    mode: String::new(),
                    detail: msg,
                }
            }
            rawler::RawlerError::DecoderFailed(msg) => RawError::Decode(msg),
        }
    }
}

fn is_unsupported_message(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("not supported") || m.contains("unsupported") || m.contains("not implemented")
}

/// Coarse failure class for catalog bookkeeping and UI copy. Serialized lowercase so it can ride
/// straight through IPC as a string discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureKind {
    Unsupported,
    Corrupt,
    Io,
    Panic,
    Other,
}

impl RawError {
    /// Coarse class of this failure — see [`FailureKind`].
    pub fn kind(&self) -> FailureKind {
        match self {
            RawError::Io(_) => FailureKind::Io,
            RawError::Decode(_) => FailureKind::Corrupt,
            RawError::Unsupported { .. } => FailureKind::Unsupported,
            RawError::DecoderPanic(_) => FailureKind::Panic,
            RawError::NoPreview | RawError::Image(_) => FailureKind::Other,
        }
    }

    /// `(make, model)` as rawler identified the body, when the error carries it.
    pub fn camera(&self) -> Option<(&str, &str)> {
        match self {
            RawError::Unsupported { make, model, .. } => Some((make.as_str(), model.as_str())),
            _ => None,
        }
    }

    /// One sentence a user can act on. rawler's own wording is accurate but opaque ("Unknown
    /// camera", "NEF compression HighEfficency is not supported"), so the cases we have actually
    /// hit in the corpus are translated here; everything else falls through to rawler's text.
    ///
    /// Both `Unsupported` and `Decode` are inspected: rawler reports Nikon HE NEFs as
    /// `DecoderFailed` (not `Unsupported`), so keying only off `Unsupported` would miss the single
    /// most common "shoot a different compression" case.
    pub fn user_detail(&self) -> String {
        let (detail, mode, camera) = match self {
            RawError::Unsupported {
                detail,
                mode,
                make,
                model,
            } => (
                detail.as_str(),
                mode.as_str(),
                Some((make.as_str(), model.as_str())),
            ),
            RawError::Decode(msg) => (msg.as_str(), "", None),
            other => return other.to_string(),
        };
        if detail.contains("HighEfficency") || detail.contains("High Efficiency") {
            return "Nikon High Efficiency (HE/HE*) NEF is not decodable — shoot Lossless compressed"
                .to_string();
        }
        // Sony's wavelet ARW is signalled in `mode` ("arw6") while `what` stays the generic
        // "Unknown camera", so this must be tested BEFORE the camera-database message below.
        if mode.eq_ignore_ascii_case("arw6") || detail.to_ascii_lowercase().contains("arw6") {
            return "Sony ARW6 compression is not supported yet".to_string();
        }
        // rawler's last-ditch failure when no container/format sniff matched: could be a damaged
        // header just as well as a format we don't know, so say so rather than blame the camera.
        if detail.contains("No decoder found") {
            return "not a recognisable RAW file (unknown format or damaged header)".to_string();
        }
        if detail.contains("Unknown camera") {
            return match camera {
                Some((make, model)) => {
                    format!("{make} {model} is not in the RAW decoder database (rawler 0.8.0)")
                }
                None => "this camera is not in the RAW decoder database (rawler 0.8.0)".to_string(),
            };
        }
        detail.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_failed_not_supported_is_promoted_to_unsupported() {
        let err: RawError = rawler::RawlerError::DecoderFailed(
            "NEF compression HighEfficencyStar is not supported".into(),
        )
        .into();
        assert_eq!(err.kind(), FailureKind::Unsupported);
        assert!(
            err.user_detail().contains("High Efficiency"),
            "{}",
            err.user_detail()
        );
        let corrupt: RawError =
            rawler::RawlerError::DecoderFailed("Can't refill bitpump, buffer exhausted".into())
                .into();
        assert_eq!(corrupt.kind(), FailureKind::Corrupt);
    }

    #[test]
    fn rawler_unsupported_maps_to_typed_unsupported() {
        let err: RawError = rawler::RawlerError::Unsupported {
            what: "NEF compression HighEfficency is not supported".into(),
            model: "Z 9".into(),
            make: "Nikon".into(),
            mode: "".into(),
        }
        .into();

        assert!(matches!(err, RawError::Unsupported { .. }));
        assert_eq!(err.kind(), FailureKind::Unsupported);
        assert_eq!(err.camera(), Some(("Nikon", "Z 9")));
        assert!(
            err.user_detail().contains("High Efficiency"),
            "user_detail should name the HE compression, got {:?}",
            err.user_detail()
        );
    }

    #[test]
    fn rawler_decoder_failed_maps_to_corrupt_decode() {
        let err: RawError = rawler::RawlerError::DecoderFailed("truncated strip".into()).into();

        assert!(matches!(err, RawError::Decode(_)));
        assert_eq!(err.kind(), FailureKind::Corrupt);
        assert_eq!(err.camera(), None);
        assert!(
            err.to_string().starts_with("decode: "),
            "Display must stay `decode: {{0}}`, got {:?}",
            err.to_string()
        );
    }

    #[test]
    fn unknown_camera_names_the_body_and_arw6_wins_over_it() {
        let unknown = RawError::Unsupported {
            make: "Nikon".into(),
            model: "Z 6III".into(),
            mode: "".into(),
            detail: "Unknown camera".into(),
        };
        let msg = unknown.user_detail();
        assert!(
            msg.contains("Nikon Z 6III") && msg.contains("rawler 0.8.0"),
            "{msg}"
        );

        // Real ARW6 files come back as `what: "Unknown camera", mode: "arw6"`.
        let arw6 = RawError::Unsupported {
            make: "Sony".into(),
            model: "ILCE-1M2".into(),
            mode: "arw6".into(),
            detail: "Unknown camera".into(),
        };
        assert_eq!(
            arw6.user_detail(),
            "Sony ARW6 compression is not supported yet"
        );
    }

    #[test]
    fn panic_and_plain_details_pass_through() {
        assert_eq!(
            RawError::DecoderPanic("develop_linear: index out of bounds".into()).kind(),
            FailureKind::Panic
        );
        assert_eq!(
            RawError::Unsupported {
                make: "Canon".into(),
                model: "EOS R7".into(),
                mode: "".into(),
                detail: "non-Bayer CFA".into(),
            }
            .user_detail(),
            "non-Bayer CFA"
        );
    }
}
