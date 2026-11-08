//! Line icons painted with egui shapes in a unit square, approximating the legacy SVG set
//! (`src/components/Icon.tsx`, 16x16 grid).

use crate::theme;
use egui::{Color32, Mesh, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Image,
    Flag,
    Trash,
    Clock,
    Folder,
    ChevronRight,
    ChevronDown,
    ChevronLeft,
    Layers,
    Plus,
    Tag,
    Scan,
    Grid,
    Square,
    Upload,
    Download,
    Gear,
    Search,
    Star,
    StarFilled,
    X,
    Sparkle,
    Person,
    Undo,
    Redo,
    Reset,
    Compare,
    Pencil,
    Eyedropper,
    RotateLeft,
    RotateRight,
}

struct Canvas<'a> {
    painter: &'a Painter,
    origin: Pos2,
    scale: f32,
    stroke: Stroke,
}

impl Canvas<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + egui::vec2(x, y) * self.scale
    }
    fn line(&self, pts: &[(f32, f32)]) {
        let points = pts.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter.add(Shape::line(points, self.stroke));
    }
    fn poly(&self, pts: &[(f32, f32)]) {
        let points = pts.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter.add(Shape::closed_line(points, self.stroke));
    }
    fn circle(&self, x: f32, y: f32, r: f32) {
        self.painter
            .circle_stroke(self.p(x, y), r * self.scale, self.stroke);
    }
    fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) {
        self.painter.rect_stroke(
            Rect::from_two_pos(self.p(x0, y0), self.p(x1, y1)),
            r * self.scale,
            self.stroke,
            StrokeKind::Middle,
        );
    }
    /// Open arc, angles in radians (0 = +x, clockwise on screen).
    fn arc(&self, x: f32, y: f32, r: f32, a0: f32, a1: f32) {
        let steps = 16;
        let pts: Vec<(f32, f32)> = (0..=steps)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / steps as f32;
                (x + r * a.cos(), y + r * a.sin())
            })
            .collect();
        self.line(&pts);
    }
}

/// Paints `icon` into `rect` (square-fit) with `color`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let size = rect.width().min(rect.height());
    let scale = size / 16.0;
    let origin = rect.center() - egui::vec2(size, size) * 0.5;
    let c = Canvas {
        painter,
        origin,
        scale,
        stroke: Stroke::new((1.3 * size / 14.0).max(1.0), color),
    };
    match icon {
        Icon::Image => {
            c.rect(2.5, 3.5, 13.5, 12.5, 1.5);
            c.circle(6.0, 7.0, 1.2);
            c.line(&[
                (3.0, 12.0),
                (6.0, 9.5),
                (9.0, 11.0),
                (11.0, 9.5),
                (13.0, 11.0),
            ]);
        }
        Icon::Flag => {
            c.line(&[(4.0, 2.5), (4.0, 13.5)]);
            c.poly(&[(4.0, 3.0), (11.0, 3.0), (9.6, 5.4), (11.0, 8.0), (4.0, 8.0)]);
        }
        Icon::Trash => {
            c.line(&[(2.5, 4.0), (13.5, 4.0)]);
            c.line(&[(6.5, 4.0), (6.5, 2.5), (9.5, 2.5), (9.5, 4.0)]);
            c.line(&[(4.0, 4.0), (4.7, 13.0), (11.3, 13.0), (12.0, 4.0)]);
            c.line(&[(6.7, 6.5), (6.7, 11.5)]);
            c.line(&[(9.3, 6.5), (9.3, 11.5)]);
        }
        Icon::Clock => {
            c.circle(8.0, 8.0, 5.5);
            c.line(&[(8.0, 5.0), (8.0, 8.0), (10.0, 9.4)]);
        }
        Icon::Folder => c.poly(&[
            (2.5, 5.0),
            (3.5, 3.5),
            (6.3, 3.5),
            (7.5, 4.9),
            (12.5, 4.9),
            (13.5, 5.9),
            (13.5, 11.5),
            (12.5, 12.5),
            (3.5, 12.5),
            (2.5, 11.5),
        ]),
        Icon::ChevronRight => c.line(&[(6.0, 4.0), (10.0, 8.0), (6.0, 12.0)]),
        Icon::ChevronDown => c.line(&[(4.0, 6.0), (8.0, 10.0), (12.0, 6.0)]),
        Icon::ChevronLeft => c.line(&[(10.0, 4.0), (6.0, 8.0), (10.0, 12.0)]),
        Icon::Layers => {
            c.poly(&[(8.0, 2.5), (13.5, 5.5), (8.0, 8.5), (2.5, 5.5)]);
            c.line(&[(2.5, 8.5), (8.0, 11.5), (13.5, 8.5)]);
        }
        Icon::Plus => {
            c.line(&[(8.0, 3.0), (8.0, 13.0)]);
            c.line(&[(3.0, 8.0), (13.0, 8.0)]);
        }
        Icon::Tag => {
            c.poly(&[
                (8.2, 2.5),
                (13.5, 2.5),
                (13.5, 7.8),
                (7.5, 13.8),
                (2.5, 8.8),
            ]);
            c.circle(10.8, 5.2, 0.9);
        }
        Icon::Scan => {
            c.rect(3.0, 3.0, 13.0, 13.0, 1.5);
            c.circle(6.0, 6.5, 1.2);
            c.line(&[(9.0, 5.5), (11.0, 5.5)]);
            c.line(&[(9.0, 7.5), (11.0, 7.5)]);
            c.line(&[(5.0, 10.0), (11.0, 10.0)]);
        }
        Icon::Grid => {
            for (x, y) in [(2.5, 2.5), (9.0, 2.5), (2.5, 9.0), (9.0, 9.0)] {
                c.rect(x, y, x + 4.5, y + 4.5, 1.0);
            }
        }
        Icon::Square => c.rect(2.5, 2.5, 13.5, 13.5, 1.5),
        Icon::Upload => {
            c.line(&[(8.0, 11.0), (8.0, 3.0)]);
            c.line(&[(5.0, 6.0), (8.0, 3.0), (11.0, 6.0)]);
            c.line(&[(3.0, 13.0), (13.0, 13.0)]);
        }
        Icon::Download => {
            c.line(&[(8.0, 2.0), (8.0, 10.0)]);
            c.line(&[(5.0, 7.0), (8.0, 10.0), (11.0, 7.0)]);
            c.line(&[(3.0, 13.0), (13.0, 13.0)]);
        }
        Icon::Gear => {
            c.circle(8.0, 8.0, 2.2);
            let pts: Vec<(f32, f32)> = (0..16)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / 16.0;
                    let r = if i % 2 == 0 { 6.6 } else { 5.0 };
                    (8.0 + r * a.cos(), 8.0 + r * a.sin())
                })
                .collect();
            c.poly(&pts);
        }
        Icon::Search => {
            c.circle(6.5, 6.5, 4.5);
            c.line(&[(10.0, 10.0), (13.5, 13.5)]);
        }
        Icon::Star | Icon::StarFilled => star(&c, icon == Icon::StarFilled, color),
        Icon::X => {
            c.line(&[(4.0, 4.0), (12.0, 12.0)]);
            c.line(&[(12.0, 4.0), (4.0, 12.0)]);
        }
        Icon::Sparkle => {
            c.poly(&[
                (8.0, 2.0),
                (9.4, 6.6),
                (14.0, 8.0),
                (9.4, 9.4),
                (8.0, 14.0),
                (6.6, 9.4),
                (2.0, 8.0),
                (6.6, 6.6),
            ]);
        }
        Icon::Person => {
            c.circle(8.0, 5.5, 2.6);
            c.arc(8.0, 14.0, 5.0, std::f32::consts::PI, std::f32::consts::TAU);
        }
        Icon::Undo => {
            c.arc(8.0, 9.0, 4.5, -2.6, 1.2);
            c.line(&[(3.0, 3.5), (3.5, 7.0), (7.0, 6.5)]);
        }
        Icon::Redo => {
            c.arc(8.0, 9.0, 4.5, 1.94, 5.74);
            c.line(&[(13.0, 3.5), (12.5, 7.0), (9.0, 6.5)]);
        }
        Icon::Reset => {
            c.arc(8.0, 8.0, 5.0, -2.2, 3.3);
            c.line(&[(3.0, 8.0), (3.0, 5.0)]);
            c.line(&[(3.0, 8.0), (6.0, 8.0)]);
        }
        Icon::Compare => {
            c.rect(2.5, 2.5, 13.5, 13.5, 1.5);
            c.line(&[(8.0, 2.5), (8.0, 13.5)]);
        }
        Icon::Pencil => c.poly(&[
            (10.5, 2.5),
            (13.5, 5.5),
            (6.0, 13.0),
            (2.5, 13.5),
            (3.0, 10.0),
        ]),
        Icon::Eyedropper => {
            c.line(&[(3.0, 13.0), (9.0, 7.0)]);
            c.line(&[(9.5, 4.5), (11.5, 6.5)]);
            c.line(&[(8.0, 6.0), (12.5, 10.5)]);
            c.line(&[(11.0, 3.5), (13.0, 5.5)]);
            c.poly(&[(3.0, 13.0), (3.0, 11.5), (4.5, 13.0)]);
        }
        Icon::RotateLeft | Icon::RotateRight => {
            let sign = if icon == Icon::RotateRight { 1.0 } else { -1.0 };
            let x = |v: f32| 8.0 + sign * (v - 8.0);
            let pts: Vec<(f32, f32)> = (0..=14)
                .map(|i| {
                    let a = -2.4 + 3.9 * i as f32 / 14.0;
                    (x(8.0 + 5.0 * a.cos()), 8.0 + 5.0 * a.sin())
                })
                .collect();
            c.line(&pts);
            let (ex, ey) = pts[pts.len() - 1];
            c.line(&[(ex, ey), (x(11.0), 10.8)]);
            c.line(&[(ex, ey), (x(13.8), 11.6)]);
        }
    }
}

/// Five-point star: outline for [`Icon::Star`], fan-filled for [`Icon::StarFilled`] (the star is
/// star-shaped around its centre, so a triangle fan fills it without a tessellator).
fn star(c: &Canvas<'_>, filled: bool, color: Color32) {
    let pts: Vec<Pos2> = (0..10)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            let r = if i % 2 == 0 { 6.2 } else { 2.9 };
            c.p(8.0 + r * a.cos(), 8.3 + r * a.sin())
        })
        .collect();
    if filled {
        let mut mesh = Mesh::default();
        mesh.colored_vertex(c.p(8.0, 8.3), color);
        for p in &pts {
            mesh.colored_vertex(*p, color);
        }
        for i in 0..10u32 {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % 10);
        }
        c.painter.add(Shape::mesh(mesh));
    }
    c.painter.add(Shape::closed_line(pts, c.stroke));
}

/// 26x26 ghost icon button with a hover tooltip.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str) -> Response {
    icon_button_enabled(ui, icon, tooltip, true)
}

/// [`icon_button`] that can be disabled: dimmed, no hover fill, `clicked()` never fires.
pub fn icon_button_enabled(
    ui: &mut egui::Ui,
    icon: Icon,
    tooltip: &str,
    enabled: bool,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), sense);
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, enabled, tooltip));
    crate::widgets::focus_ring(ui, &response, rect, theme::RADIUS_SM);
    if !enabled {
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_disabled();
        });
        paint(
            ui.painter(),
            Rect::from_center_size(rect.center(), egui::vec2(15.0, 15.0)),
            icon,
            theme::T3,
        );
        return response.on_hover_text(tooltip);
    }
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, theme::RADIUS_SM, theme::HOVER);
    }
    let color = if response.hovered() {
        theme::T1
    } else {
        theme::T2
    };
    paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), egui::vec2(15.0, 15.0)),
        icon,
        color,
    );
    response.on_hover_text(tooltip)
}
