//! 256-bin per-channel histogram computed from a rendered RGBA8 buffer, for the develop UI.

use serde::Serialize;

/// Per-channel 256-bin counts (one bin per 8-bit level). Sent to the frontend after each render.
#[derive(Debug, Clone, Serialize)]
pub struct Histogram {
    pub r: Vec<u32>,
    pub g: Vec<u32>,
    pub b: Vec<u32>,
}

/// Count R/G/B levels of a tightly-packed RGBA8 buffer (`w*h*4`). Alpha is ignored.
pub fn histogram(rgba: &[u8]) -> Histogram {
    let mut r = vec![0u32; 256];
    let mut g = vec![0u32; 256];
    let mut b = vec![0u32; 256];
    for px in rgba.chunks_exact(4) {
        r[px[0] as usize] += 1;
        g[px[1] as usize] += 1;
        b[px[2] as usize] += 1;
    }
    Histogram { r, g, b }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_levels_per_channel() {
        // Two pixels: (10, 20, 30) and (10, 40, 30).
        let rgba = [10, 20, 30, 255, 10, 40, 30, 255];
        let h = histogram(&rgba);
        assert_eq!(h.r[10], 2);
        assert_eq!(h.g[20], 1);
        assert_eq!(h.g[40], 1);
        assert_eq!(h.b[30], 2);
        assert_eq!(h.r.iter().sum::<u32>(), 2);
    }
}
