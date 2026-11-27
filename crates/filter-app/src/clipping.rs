//! Viewport clipping overlay state: shadow and highlight flags drawn over the
//! canvas. Pure view state, outside `EditState` and undo history; the histogram,
//! cursor probe and export are unaffected.
use eframe::egui;

/// Toggles both overlays; ignored while a text field has focus.
pub const TOGGLE_KEY: egui::Key = egui::Key::J;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClippingOverlay {
    pub shadows: bool,
    pub highlights: bool,
}
impl ClippingOverlay {
    /// If either overlay is off turn both on, otherwise turn both off.
    pub fn toggle_all(&mut self) {
        let on = !(self.shadows && self.highlights);
        self.shadows = on;
        self.highlights = on;
    }
    /// The two checkboxes, with the shortcut in their tooltips.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.shadows, "Shadow clipping")
                .on_hover_text("Blue where any channel is at black · J toggles both");
            ui.checkbox(&mut self.highlights, "Highlight clipping")
                .on_hover_text("Red where any channel is at white · J toggles both");
        });
    }
}

#[cfg(test)]
#[path = "../tests/unit/clipping_tests.rs"]
mod tests;
