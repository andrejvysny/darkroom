//! Right info panel: preview, rating / flag / colour controls, histogram, metadata.

use super::{cull_target, selected_row, INFO_ID, NOT_YET};
use crate::widgets::{chip, color_dots, placeholder_row, star_rating};
use crate::{theme, HistogramData, ThumbSource, ThumbState};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{ColorLabel, Command, CullChange, CullFlag};
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;
use egui::{Align, Color32, Layout, Pos2, Rect, RichText, Sense};

pub(crate) fn show(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    thumbs: &mut dyn ThumbSource,
    histogram: Option<&HistogramData>,
    commands: &mut Vec<Command>,
) {
    egui::Panel::right(super::INFO_ID)
        .exact_size(240.0)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::same(12)),
        )
        .show(root, |ui| {
            ui.push_id(INFO_ID, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let Some(row) = selected_row(snapshot) else {
                            ui.add_space(24.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new("Select a photo").color(theme::T3));
                            });
                            return;
                        };
                        preview(ui, row, thumbs);
                        rating(ui, snapshot, row, commands);
                        histogram_box(ui, row, histogram);
                        metadata(ui, row);
                        theme::section_heading(ui, "Keywords");
                        placeholder_row(ui, NOT_YET);
                    });
            });
        });
}

fn preview(ui: &mut egui::Ui, row: &ImageRow, thumbs: &mut dyn ThumbSource) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, width * 0.75), Sense::hover());
    ui.painter()
        .rect_filled(rect, theme::RADIUS_SM, theme::STAGE);
    if let ThumbState::Ready { texture, size } = thumbs.thumb(row) {
        let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
        let scale = (rect.width() / w).min(rect.height() / h);
        let fit = Rect::from_center_size(rect.center(), egui::vec2(w * scale, h * scale));
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        ui.painter().image(texture, fit, uv, Color32::WHITE);
    }
}

fn rating(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    row: &ImageRow,
    commands: &mut Vec<Command>,
) {
    let target = cull_target(snapshot);
    let mut cull = |image: ImageId, change: CullChange| {
        commands.push(Command::SetCull { image, change });
    };
    theme::section_heading(ui, "Rating");
    let stars = u8::try_from(row.stars.clamp(0, 5)).unwrap_or(0);
    if let Some(n) = star_rating(ui, stars, 16.0, target.is_some()) {
        if let Some(image) = target {
            cull(image, CullChange::Rating(n));
        }
    }
    let flag = CullFlag::from_row(&row.flag);
    ui.horizontal(|ui| {
        for (label, mine) in [("Pick", CullFlag::Pick), ("Reject", CullFlag::Reject)] {
            let active = flag == mine;
            if chip(ui, label, active).clicked() {
                if let Some(image) = target {
                    let next = if active { CullFlag::None } else { mine };
                    cull(image, CullChange::Flag(next));
                }
            }
        }
    });
    let label = row.color_label.as_deref().and_then(ColorLabel::from_row);
    if let Some(next) = color_dots(ui, label, 10.0) {
        if let Some(image) = target {
            cull(image, CullChange::Label(next));
        }
    }
}

fn histogram_box(ui: &mut egui::Ui, row: &ImageRow, histogram: Option<&HistogramData>) {
    theme::section_heading(ui, "Histogram");
    crate::histogram::draw(ui, 64.0, histogram, row.id);
}

fn metadata(ui: &mut egui::Ui, row: &ImageRow) {
    theme::section_heading(ui, "Metadata");
    let dash = || "—".to_owned();
    let camera = row.camera_model.clone().or_else(|| row.camera_make.clone());
    let size = match (row.width, row.height) {
        (Some(w), Some(h)) => format!("{w}×{h}"),
        _ => dash(),
    };
    let fields = [
        ("Camera", camera.unwrap_or_else(dash)),
        ("Lens", row.lens.clone().unwrap_or_else(dash)),
        (
            "Focal",
            row.focal_length.map_or_else(dash, |f| format!("{f:.0} mm")),
        ),
        (
            "Aperture",
            row.aperture.map_or_else(dash, |a| format!("f/{a}")),
        ),
        ("Shutter", row.shutter.clone().unwrap_or_else(dash)),
        ("ISO", row.iso.map_or_else(dash, |i| i.to_string())),
        ("Size", size),
        ("Date", row.capture_date.map_or_else(dash, format_date)),
        ("File", row.filename.clone()),
    ];
    for (label, value) in fields {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).size(11.5).color(theme::T3));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(RichText::new(value).monospace().size(11.0).color(theme::T1))
                        .truncate(),
                );
            });
        });
    }
}

/// Epoch seconds -> UTC `YYYY-MM-DD` (Hinnant's civil-from-days).
pub(crate) fn format_date(epoch_seconds: i64) -> String {
    let z = epoch_seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::format_date;

    #[test]
    fn civil_dates() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_783_123_200), "2026-07-04");
        assert_eq!(format_date(951_782_400), "2000-02-29");
        assert_eq!(format_date(-86_400), "1969-12-31");
    }
}
