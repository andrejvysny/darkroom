//! Colour tokens and egui style mirroring the legacy `src/styles.css` (dark, Lightroom-like).

use egui::{Color32, CornerRadius, FontFamily, FontId, RichText, Stroke, TextStyle};

pub const STAGE: Color32 = Color32::from_rgb(0x12, 0x12, 0x13);
pub const STAGE_DEV: Color32 = Color32::from_rgb(0x10, 0x10, 0x11);
pub const APP: Color32 = Color32::from_rgb(0x1b, 0x1b, 0x1d);
pub const PANEL: Color32 = Color32::from_rgb(0x23, 0x23, 0x25);
pub const ELEV: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2e);
pub const HOVER: Color32 = Color32::from_rgb(0x30, 0x30, 0x34);
pub const T1: Color32 = Color32::from_rgb(0xe9, 0xe9, 0xec);
pub const T2: Color32 = Color32::from_rgb(0x9c, 0x9c, 0xa3);
pub const T3: Color32 = Color32::from_rgb(0x66, 0x66, 0x6c);
pub const ACCENT: Color32 = Color32::from_rgb(0x5b, 0x93, 0xcf);
pub const ACCENT_DIM: Color32 = Color32::from_rgba_premultiplied(15, 24, 34, 41);
pub const ACCENT_LINE: Color32 = Color32::from_rgba_premultiplied(50, 81, 114, 140);
pub const PICK: Color32 = Color32::from_rgb(0x6d, 0xb0, 0x74);
pub const REJECT: Color32 = Color32::from_rgb(0xc5, 0x6d, 0x6d);
pub const STAR: Color32 = Color32::from_rgb(0xd8, 0xb1, 0x5a);
pub const LAB_RED: Color32 = Color32::from_rgb(0xc5, 0x6d, 0x6d);
pub const LAB_YELLOW: Color32 = Color32::from_rgb(0xcd, 0xb9, 0x5a);
pub const LAB_GREEN: Color32 = Color32::from_rgb(0x6d, 0xb0, 0x74);
pub const LAB_BLUE: Color32 = Color32::from_rgb(0x5b, 0x93, 0xcf);
pub const LAB_PURPLE: Color32 = Color32::from_rgb(0x9a, 0x7f, 0xc0);
pub const LINE: Color32 = Color32::from_rgba_premultiplied(18, 18, 18, 18);
pub const LINE2: Color32 = Color32::from_rgba_premultiplied(31, 31, 31, 31);

pub const RADIUS_SM: u8 = 6;
pub const RADIUS_MD: u8 = 9;
pub const RADIUS_LG: u8 = 12;

/// Applies the theme once per context (a temp-data flag; cheap to call every frame).
pub fn ensure(ctx: &egui::Context) {
    let flag = egui::Id::unique("darkroom_theme_applied");
    if ctx.data(|d| d.get_temp::<bool>(flag)).is_some() {
        return;
    }
    ctx.set_visuals(visuals());
    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(11.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(11.5, FontFamily::Monospace),
            ),
            (
                TextStyle::Heading,
                FontId::new(14.0, FontFamily::Proportional),
            ),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
    });
    ctx.data_mut(|d| d.insert_temp(flag, true));
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = STAGE;
    v.faint_bg_color = ELEV;
    v.override_text_color = None;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.window_stroke = Stroke::new(1.0, LINE2);
    let radius = CornerRadius::same(RADIUS_SM);
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, T1);
    for (w, fill) in [
        (&mut v.widgets.inactive, ELEV),
        (&mut v.widgets.hovered, HOVER),
        (&mut v.widgets.active, HOVER),
        (&mut v.widgets.open, HOVER),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, LINE2);
        w.fg_stroke = Stroke::new(1.0, T1);
        w.corner_radius = radius;
    }
    v.widgets.noninteractive.corner_radius = radius;
    v.menu_corner_radius = radius;
    v.window_corner_radius = CornerRadius::same(RADIUS_MD);
    v
}

/// Small uppercase section heading with extra top spacing (nav / info panel).
pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .size(10.5)
            .color(T3)
            .extra_letter_spacing(1.0),
    );
    ui.add_space(2.0);
}

/// Monospace text in the secondary colour.
pub fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).monospace().color(T2)
}
