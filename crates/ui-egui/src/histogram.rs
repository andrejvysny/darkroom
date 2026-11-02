//! Shared histogram box: overlapping R/G/B filled areas, sqrt-scaled (legacy `Histogram.tsx`).

use crate::{theme, HistogramData};
use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};

fn tints() -> [Color32; 3] {
    [
        Color32::from_rgba_unmultiplied(197, 109, 109, 140),
        Color32::from_rgba_unmultiplied(109, 176, 116, 140),
        Color32::from_rgba_unmultiplied(91, 147, 207, 140),
    ]
}

/// Largest bin ignoring the pure-black/white end bins (their spikes would flatten the rest).
pub(crate) fn scale_max(channels: [&[u32]; 3]) -> f32 {
    channels
        .iter()
        .flat_map(|c| c.iter().enumerate())
        .filter(|(i, _)| *i > 0)
        .filter(|(i, _)| channels.iter().all(|c| *i + 1 < c.len()))
        .map(|(_, &v)| v)
        .max()
        .unwrap_or(1)
        .max(1) as f32
}

/// Triangle-strip area under one channel, `[0, 1]` normalised heights.
pub(crate) fn area_mesh(bins: &[u32], peak: f32, rect: Rect, color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    let n = bins.len();
    if n < 2 {
        return mesh;
    }
    for (i, &count) in bins.iter().enumerate() {
        let x = rect.left() + rect.width() * i as f32 / (n - 1) as f32;
        let h = (count as f32 / peak).sqrt().min(1.0) * rect.height();
        for y in [rect.bottom(), rect.bottom() - h] {
            mesh.vertices.push(Vertex {
                pos: Pos2::new(x, y),
                uv: WHITE_UV,
                color,
            });
        }
    }
    for i in 0..(n as u32 - 1) {
        let b = 2 * i;
        mesh.add_triangle(b, b + 1, b + 2);
        mesh.add_triangle(b + 1, b + 3, b + 2);
    }
    mesh
}

/// Draws the box at `height`. `image` is the id the histogram must belong to; anything else
/// (or `None`) shows an empty box, never another photo's distribution.
pub(crate) fn draw(ui: &mut Ui, height: f32, histogram: Option<&HistogramData>, image: i64) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, theme::RADIUS_SM, theme::STAGE_DEV);
    match histogram.filter(|h| h.image.0 == image) {
        Some(h) => {
            let peak = scale_max([&h.r, &h.g, &h.b]);
            let inner = rect.shrink(1.0);
            for (bins, tint) in [&h.r, &h.g, &h.b].into_iter().zip(tints()) {
                painter.add(area_mesh(bins, peak, inner, tint));
            }
        }
        None => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "—",
                FontId::proportional(14.0),
                theme::T3,
            );
        }
    }
    painter.rect_stroke(
        rect,
        theme::RADIUS_SM,
        Stroke::new(1.0, theme::LINE),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::{area_mesh, scale_max};
    use egui::{Color32, Pos2, Rect};

    #[test]
    fn peak_ignores_end_bins() {
        let r = [900, 3, 5, 900];
        assert_eq!(scale_max([&r, &r, &r]), 5.0);
    }

    #[test]
    fn mesh_is_a_strip_two_triangles_per_segment() {
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(100.0, 50.0));
        let mesh = area_mesh(&[0, 4, 4], 4.0, rect, Color32::RED);
        assert_eq!(mesh.vertices.len(), 6);
        assert_eq!(mesh.indices.len(), 12);
        // Full-height bin reaches the top of the rect.
        assert_eq!(mesh.vertices[3].pos.y, 0.0);
    }
}
