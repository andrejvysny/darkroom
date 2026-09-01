//! CLIP embedding stage — persists the full-image MobileCLIP-S1 vector for downstream models.
//!
//! Where [`crate::presence::PresenceProbe`] scores the embedding and throws it away, this stage keeps
//! it: the 512-d feature is the input a pick/reject (or any other per-image) model is trained and
//! served on, and re-running CLIP over a whole library to get it back costs hours. Reuses the already-
//! loaded verifier vision encoder ([`Verifier::embed_full`]) — no extra model file, no extra download.
//!
//! The vector rides the payload as **hex-encoded f32 little-endian bytes**, not a JSON float array:
//! those bytes are byte-for-byte what `core-library` stores in the `image_embedding` BLOB, so the
//! catalog layer never parses a float and cannot lose a bit to decimal round-tripping. The payload is
//! transient — `insert_analysis` projects it into `image_embedding` and keeps only a `{"dim":512}`
//! marker in `analysis_results`.

use std::sync::Arc;

use crate::error::AnalyzeError;
use crate::{AnalysisCtx, AnalysisRecord, Analyzer, Verifier};

pub struct EmbeddingStage {
    verifier: Arc<Verifier>,
    model_version: &'static str,
}

impl EmbeddingStage {
    /// Bind the shared verifier. `model_version` gates re-analysis and is also stored as the row's
    /// `model_tag`, so swapping the encoder invalidates every stored vector.
    pub fn new(verifier: Arc<Verifier>, model_version: &'static str) -> Self {
        Self {
            verifier,
            model_version,
        }
    }
}

impl Analyzer for EmbeddingStage {
    fn id(&self) -> &'static str {
        crate::CLIP_EMBEDDING_ID
    }

    fn model_version(&self) -> &'static str {
        self.model_version
    }

    fn analyze(&self, ctx: &AnalysisCtx) -> Result<AnalysisRecord, AnalyzeError> {
        let emb = self.verifier.embed_full(ctx.image)?;
        // Caught here rather than in the catalog projection so a broken encoder is recorded as this
        // stage's error attempt (and retried next run) instead of aborting the whole scan's write.
        if emb.is_empty() {
            return Err(AnalyzeError::Other(
                "CLIP vision encoder returned no embedding".into(),
            ));
        }
        let payload = serde_json::to_value(EmbeddingPayload {
            dim: emb.len(),
            vector_hex: to_hex(&emb),
        })
        .map_err(|e| AnalyzeError::Other(e.to_string()))?;
        Ok(AnalysisRecord::new(self.id(), self.model_version, payload))
    }
}

/// Transient carrier for the vector between the analyzer and the catalog projection. Field names are
/// mirrored (not shared) by `core_library::analysis` — that crate deliberately links no ML code.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingPayload {
    pub dim: usize,
    pub vector_hex: String,
}

/// f32 slice → lowercase hex of its little-endian bytes (`dim * 8` chars).
fn to_hex(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8);
    for x in v {
        for b in x.to_le_bytes() {
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
            s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
        }
    }
    s
}

/// Inverse of [`to_hex`]. `None` for anything that isn't a whole number of little-endian f32s.
pub fn from_hex(hex: &str) -> Option<Vec<f32>> {
    if !hex.len().is_multiple_of(8) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 8);
    for c in hex.as_bytes().chunks_exact(8) {
        let mut b = [0u8; 4];
        for (i, pair) in c.chunks_exact(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            b[i] = (hi * 16 + lo) as u8;
        }
        out.push(f32::from_le_bytes(b));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_every_bit() {
        // The values that break a decimal round-trip: denormals, tiny deltas, and the sign of zero.
        let v: Vec<f32> = vec![0.0, -0.0, 1.0, -1.0, f32::MIN_POSITIVE, 0.1, 0.100000024];
        let hex = to_hex(&v);
        assert_eq!(hex.len(), v.len() * 8);
        let back = from_hex(&hex).unwrap();
        assert_eq!(back.len(), v.len());
        for (a, b) in v.iter().zip(&back) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn malformed_hex_is_rejected() {
        assert!(from_hex("abc").is_none(), "not a whole f32");
        assert!(from_hex("zzzzzzzz").is_none(), "not hex");
        assert_eq!(from_hex("").unwrap(), Vec::<f32>::new());
    }
}
