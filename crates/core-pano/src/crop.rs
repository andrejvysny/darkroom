//! Auto-crop: the largest axis-aligned all-covered rectangle inside the valid mask.
//!
//! Classic "maximal rectangle of 1s": maintain, per column, the running height of consecutive covered
//! pixels ending at the current row, then for each row solve the largest-rectangle-in-histogram with a
//! monotonic stack. Overall `O(w·h)`.

/// Largest inscribed rectangle of covered pixels. Returns `(x0, y0, cw, ch)`; a degenerate all-empty
/// mask yields `(0, 0, 0, 0)`.
pub(crate) fn largest_inscribed_rect(
    valid: &[u8],
    w: usize,
    h: usize,
) -> (usize, usize, usize, usize) {
    if w == 0 || h == 0 {
        return (0, 0, 0, 0);
    }
    let mut heights = vec![0usize; w];
    let mut best = (0usize, 0usize, 0usize, 0usize); // (x0, y0, cw, ch)
    let mut best_area = 0usize;

    for y in 0..h {
        for x in 0..w {
            heights[x] = if valid[y * w + x] != 0 {
                heights[x] + 1
            } else {
                0
            };
        }
        // Largest rectangle in this row's histogram; the rectangle's bottom edge is row `y`.
        let (rx0, rw, rh, area) = largest_in_histogram(&heights);
        if area > best_area {
            best_area = area;
            best = (rx0, y + 1 - rh, rw, rh);
        }
    }
    best
}

/// Largest rectangle in a histogram: returns `(x0, width, height, area)` of the best bar-bounded rect.
fn largest_in_histogram(heights: &[usize]) -> (usize, usize, usize, usize) {
    let mut stack: Vec<usize> = Vec::with_capacity(heights.len() + 1);
    let mut best = (0usize, 0usize, 0usize, 0usize);
    let mut best_area = 0usize;
    let n = heights.len();
    for i in 0..=n {
        let cur = if i < n { heights[i] } else { 0 };
        while let Some(&top) = stack.last() {
            if heights[top] <= cur {
                break;
            }
            stack.pop();
            let height = heights[top];
            let left = stack.last().map(|&s| s + 1).unwrap_or(0);
            let width = i - left;
            let area = height * width;
            if area > best_area {
                best_area = area;
                best = (left, width, height, area);
            }
        }
        stack.push(i);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_mask_is_whole_rect() {
        let (w, h) = (12, 8);
        let valid = vec![1u8; w * h];
        assert_eq!(largest_inscribed_rect(&valid, w, h), (0, 0, 12, 8));
    }

    #[test]
    fn l_shape() {
        // 12x8: left 4 columns empty on the top 4 rows -> the maximal rect is the bottom 4 rows
        // (12x4 = 48) vs the right 8 columns full height (8x8 = 64). Expect the right block.
        let (w, h) = (12usize, 8usize);
        let mut valid = vec![1u8; w * h];
        for y in 0..4 {
            for x in 0..4 {
                valid[y * w + x] = 0;
            }
        }
        let (x0, y0, cw, ch) = largest_inscribed_rect(&valid, w, h);
        assert_eq!((x0, y0, cw, ch), (4, 0, 8, 8));
    }

    #[test]
    fn diagonal() {
        // Zero the main diagonal of a 12x8 mask; the largest all-ones rect avoids the diagonal.
        let (w, h) = (12usize, 8usize);
        let mut valid = vec![1u8; w * h];
        for d in 0..h {
            valid[d * w + d] = 0;
        }
        let (_, _, cw, ch) = largest_inscribed_rect(&valid, w, h);
        // Below the diagonal: rows y have columns 0..y available (y up to 7) -> best there is the
        // block to the RIGHT of the diagonal: columns d+1..12 across all rows gives 12-1=11 wide by...
        // Simplest invariant: area is positive and strictly smaller than the full 96.
        assert!(cw * ch > 0 && cw * ch < 96, "got {cw}x{ch}");
        // The right-of-diagonal block spans columns 8..12 (width 4) for all 8 rows -> 32; ensure the
        // result is at least that large.
        assert!(cw * ch >= 32, "expected >= 32, got {cw}x{ch}");
    }
}
