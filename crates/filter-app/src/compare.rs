//! Before/after comparison on the canvas: a draggable split, or the original
//! across the whole canvas while the shortcut is held. Only the viewport changes;
//! the histogram, cursor probe and export keep describing the edited image.
use eframe::egui;

/// Pointer tolerance on each side of the divider, in points.
const GRAB: f32 = 6.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Compare {
    /// Divider position as a fraction of the canvas width; `None` shows the edit.
    pub split: Option<f32>,
    /// While held, the whole canvas shows the original.
    pub held: bool,
}
impl Compare {
    pub const DEFAULT_SPLIT: f32 = 0.5;

    /// Divider x in points, on the physical pixel boundary that
    /// [`Self::before_until`] uses, so the line and the image change agree.
    pub fn divider_x(&self, viewport: egui::Rect, pixels_per_point: f32) -> Option<f32> {
        let fraction = self.split?;
        let x = viewport.min.x + fraction.clamp(0.0, 1.0) * viewport.width();
        Some((x * pixels_per_point).round() / pixels_per_point)
    }
    /// Physical framebuffer x left of which the renderer shows the original.
    pub fn before_until(&self, viewport: egui::Rect, pixels_per_point: f32) -> f32 {
        if self.held {
            f32::MAX
        } else {
            self.divider_x(viewport, pixels_per_point)
                .map_or(0.0, |x| x * pixels_per_point)
        }
    }
    /// Let the divider be dragged. Call after allocating the canvas, so the
    /// divider lies above it and its drags do not pan the image.
    pub fn interact(&mut self, ui: &egui::Ui, viewport: egui::Rect) {
        let Some(x) = self.divider_x(viewport, ui.ctx().pixels_per_point()) else {
            return;
        };
        if self.held || !viewport.is_positive() {
            return;
        }
        let handle = egui::Rect::from_x_y_ranges(x - GRAB..=x + GRAB, viewport.y_range())
            .intersect(viewport);
        let response = ui
            .interact(
                handle,
                ui.scope_id().with("compare divider"),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            if let Some(pointer) = response.interact_pointer_pos() {
                self.split =
                    Some(((pointer.x - viewport.min.x) / viewport.width()).clamp(0.0, 1.0));
            }
        }
    }
    /// Paint the divider and Before/After labels over the image.
    pub fn paint(&self, ui: &egui::Ui, viewport: egui::Rect) {
        let painter = ui.painter().with_clip_rect(viewport);
        let top = viewport.min.y + 8.0;
        if self.held {
            label(
                &painter,
                egui::pos2(viewport.center().x, top),
                egui::Align::Center,
                "Before",
            );
            return;
        }
        let Some(x) = self.divider_x(viewport, ui.ctx().pixels_per_point()) else {
            return;
        };
        let stroke = |width, gray| egui::Stroke::new(width, egui::Color32::from_gray(gray));
        painter.vline(x, viewport.y_range(), stroke(3.0, 16));
        painter.vline(x, viewport.y_range(), stroke(1.0, 235));
        let knob = egui::pos2(x, viewport.center().y);
        painter.circle(
            knob,
            GRAB + 1.0,
            egui::Color32::from_gray(235),
            stroke(1.5, 16),
        );
        label(
            &painter,
            egui::pos2(x - 8.0, top),
            egui::Align::Max,
            "Before",
        );
        label(
            &painter,
            egui::pos2(x + 8.0, top),
            egui::Align::Min,
            "After",
        );
    }
}

/// A small label on a dark plate, aligned horizontally to `anchor` at its top.
fn label(painter: &egui::Painter, anchor: egui::Pos2, align: egui::Align, text: &str) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
    );
    let size = galley.size() + egui::vec2(12.0, 6.0);
    let left = match align {
        egui::Align::Min => anchor.x,
        egui::Align::Center => anchor.x - size.x / 2.0,
        egui::Align::Max => anchor.x - size.x,
    };
    let plate = egui::Rect::from_min_size(egui::pos2(left, anchor.y), size);
    painter.rect_filled(plate, 4.0, egui::Color32::from_black_alpha(170));
    painter.galley(
        plate.min + egui::vec2(6.0, 3.0),
        galley,
        egui::Color32::WHITE,
    );
}

#[cfg(test)]
#[path = "../tests/unit/compare_tests.rs"]
mod tests;
