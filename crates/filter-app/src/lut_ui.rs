//! Look (LUT) section: load a `.cube` file, show its title and size, set the
//! strength and remove it. The first Finishing control; vignette and grain have
//! no controls. The LUT lives in `Finishing.lut` and `lut_strength`, so every
//! action here is one ordinary undo step.
use eframe::egui;
use filter_core::{
    edit::Finishing,
    lut::{CubeLut, LutSlot},
};
use std::sync::Arc;

/// Longest error text shown, in characters. Parser messages are short; this
/// bounds anything unexpected such as a long OS error.
const MAX_STATUS_CHARS: usize = 300;

/// A parsed file and the name to show if the LUT has no `TITLE`.
pub struct Loaded {
    pub lut: CubeLut,
    pub file_name: String,
}
/// What the section asks the app to do after a frame. Both can be set in one
/// frame (a strength drag released with a click on Replace).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Actions {
    /// Open the file dialog.
    pub load: bool,
    /// The edit changed (strength or removal): refresh the renderer snapshot.
    pub changed: bool,
}

/// App state, not part of the edit or its undo history.
#[derive(Default)]
pub struct LutUi {
    pub status: String,
    /// File name of the last loaded LUT, used only while that same LUT is active.
    named: Option<(Arc<CubeLut>, String)>,
}
impl LutUi {
    pub fn loading(&mut self) {
        self.status = "Loading LUT…".into();
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Apply a finished load to `finishing`; returns whether the edit changed.
    /// A result for an older image is dropped. A failure leaves the edit alone.
    pub fn finish_load(
        &mut self,
        same_image: bool,
        result: Option<Result<Box<Loaded>, String>>,
        finishing: &mut Finishing,
    ) -> bool {
        self.status.clear();
        if !same_image {
            return false;
        }
        match result {
            Some(Ok(loaded)) => self.apply(*loaded, finishing),
            Some(Err(error)) => {
                let text: String = error.chars().take(MAX_STATUS_CHARS).collect();
                self.status = format!("Could not load LUT: {text}");
                false
            }
            None => false,
        }
    }
    /// Set the LUT, reusing the current `Arc` for an identical one so it makes no
    /// undo step and no GPU upload. A first LUT starts at full strength; a
    /// replacement keeps the current strength.
    fn apply(&mut self, loaded: Loaded, finishing: &mut Finishing) -> bool {
        let slot = LutSlot::Cube(Arc::new(loaded.lut)).interned(&finishing.lut);
        if slot == finishing.lut {
            return false;
        }
        if finishing.lut == LutSlot::None {
            finishing.lut_strength = 1.0;
        }
        if let Some(lut) = slot.cube() {
            self.named = Some((lut.clone(), loaded.file_name));
        }
        finishing.lut = slot;
        true
    }

    /// Returns the actions the app must take. Strength drags and typed
    /// values are single gestures for undo, like the other sliders.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        finishing: &mut Finishing,
        can_load: bool,
    ) -> Actions {
        let mut actions = Actions::default();
        match finishing.lut.cube().cloned() {
            None => {
                actions.load = ui
                    .add_enabled(can_load, egui::Button::new("Load .cube…"))
                    .clicked();
            }
            Some(lut) => actions = self.loaded_controls(ui, finishing, &lut, can_load),
        }
        if !self.status.is_empty() {
            ui.add(egui::Label::new(self.status.as_str()).wrap());
        }
        ui.weak("Expects sRGB-encoded input and output. Log or video-range LUTs will look wrong.");
        actions
    }
    fn loaded_controls(
        &mut self,
        ui: &mut egui::Ui,
        finishing: &mut Finishing,
        lut: &Arc<CubeLut>,
        can_load: bool,
    ) -> Actions {
        ui.label(self.display_name(lut));
        ui.weak(describe(lut));
        let mut changed = strength_slider(ui, &mut finishing.lut_strength);
        let mut load = false;
        ui.horizontal(|ui| {
            load = ui
                .add_enabled(can_load, egui::Button::new("Replace…"))
                .clicked();
            if ui.button("Remove").clicked() {
                self.remove(finishing);
                changed = true;
            }
        });
        Actions { load, changed }
    }
    /// Clear the LUT and everything that referenced it, so the `Arc` is not kept alive.
    fn remove(&mut self, finishing: &mut Finishing) {
        remove(finishing);
        self.status.clear();
        self.named = None;
    }
    fn display_name(&self, lut: &Arc<CubeLut>) -> String {
        if !lut.title().is_empty() {
            return lut.title().to_owned();
        }
        match &self.named {
            Some((named, file)) if Arc::ptr_eq(named, lut) => file.clone(),
            _ => "Untitled LUT".into(),
        }
    }
}

/// Clear the LUT and restore the default strength, so a neutral edit equals the default.
pub fn remove(finishing: &mut Finishing) {
    finishing.lut = LutSlot::None;
    finishing.lut_strength = 1.0;
}
/// `N³`, plus the input domain when it is not 0..1.
pub fn describe(lut: &CubeLut) -> String {
    let n = lut.size();
    let mut text = format!("{n}³");
    if lut.domain_min() != [0.0; 3] || lut.domain_max() != [1.0; 3] {
        text += &format!(" · domain {:?} to {:?}", lut.domain_min(), lut.domain_max());
    }
    text
}
/// Strength text: the 0..1 value shown as 0..100 percent.
pub fn percent(value: f64) -> String {
    format!("{:.0}", value * 100.0)
}
/// Typed percent back to a 0..1 strength, limited to the slider range.
pub fn parse_percent(text: &str) -> Option<f64> {
    text.trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| (v / 100.0).clamp(0.0, 1.0))
}

fn strength_slider(ui: &mut egui::Ui, strength: &mut f32) -> bool {
    let response = ui.add(
        egui::Slider::new(strength, 0.0..=1.0)
            .text("Strength %")
            .custom_formatter(|v, _| percent(v))
            .custom_parser(parse_percent)
            .update_while_editing(false),
    );
    // Double-click returns the slider to full strength.
    let reset = response.double_clicked() && *strength != 1.0;
    if reset {
        *strength = 1.0;
    }
    response.changed() || reset
}

#[cfg(test)]
#[path = "../tests/unit/lut_ui_tests.rs"]
mod tests;
