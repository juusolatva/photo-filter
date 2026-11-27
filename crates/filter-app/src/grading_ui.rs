//! Color grading: shadows, midtones and highlights wheels as sliders (hue,
//! chroma, lightness) plus the balance control. A 2D wheel widget can write the
//! same fields later.
use crate::mixer_ui::{paint_gradient, percent, swatch};
use eframe::egui;
use filter_core::edit::{Color, GradingWheel};
use std::sync::OnceLock;

pub const WHEEL_NAMES: [&str; 3] = ["Shadows", "Midtones", "Highlights"];
/// Rainbow track stops across the hue slider (turns 0..1).
const RAINBOW_STOPS: usize = 12;
/// Thickness of the gradient strip under each slider.
const TRACK: f32 = 3.0;

/// App state, not part of the edit or its undo history.
#[derive(Default)]
pub struct GradingUi {
    pub tab: usize,
}

/// Slider text for a hue in turns: degrees of OkLCh hue.
pub fn degrees_text(turns: f64) -> String {
    format!("{:.0}°", turns * 360.0)
}
/// Typed degrees (a trailing degree sign is allowed) back to turns in 0..1.
/// Hue is circular, so values wrap: 370 is 10 degrees and -10 is 350.
pub fn parse_degrees(text: &str) -> Option<f64> {
    text.trim()
        .trim_end_matches('°')
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| (v / 360.0).rem_euclid(1.0))
}
/// Chroma shown as 0..100.
pub fn chroma_text(value: f64) -> String {
    format!("{:.0}", value * 100.0)
}
/// Typed chroma percent back to 0..1.
pub fn parse_chroma(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| (v / 100.0).clamp(0.0, 1.0))
}
fn parse_signed(text: &str) -> Option<f64> {
    crate::mixer_ui::parse_percent(text)
}
/// Whether a wheel differs from its neutral state.
pub fn wheel_active(wheel: &GradingWheel) -> bool {
    *wheel != GradingWheel::default()
}

impl GradingUi {
    /// Returns whether the color changed. Sliders apply typed values once on
    /// Enter or focus loss; drags and resets are single gestures for undo.
    pub fn show(&mut self, ui: &mut egui::Ui, color: &mut Color) -> bool {
        ui.horizontal_wrapped(|ui| {
            for (index, name) in WHEEL_NAMES.iter().enumerate() {
                ui.selectable_value(&mut self.tab, index, *name);
            }
        });
        self.tab = self.tab.min(WHEEL_NAMES.len() - 1);
        let mut changed = wheel_sliders(ui, &mut color.grading[self.tab]);
        changed |= slider(
            ui,
            &mut color.scalars.grading_balance,
            "Balance",
            Spec::SIGNED,
            None,
        );
        changed | self.reset_buttons(ui, color)
    }
    fn reset_buttons(&self, ui: &mut egui::Ui, color: &mut Color) -> bool {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            let wheel = &mut color.grading[self.tab];
            let label = format!("Reset {}", WHEEL_NAMES[self.tab].to_lowercase());
            if ui
                .add_enabled(wheel_active(wheel), egui::Button::new(label))
                .clicked()
            {
                *wheel = GradingWheel::default();
                changed = true;
            }
            let active =
                color.grading.iter().any(wheel_active) || color.scalars.grading_balance != 0.0;
            if ui
                .add_enabled(active, egui::Button::new("Reset grading"))
                .clicked()
            {
                color.grading = Default::default();
                color.scalars.grading_balance = 0.0;
                changed = true;
            }
        });
        changed
    }
}

fn wheel_sliders(ui: &mut egui::Ui, wheel: &mut GradingWheel) -> bool {
    let mut changed = slider(ui, &mut wheel.hue, "Hue", Spec::HUE, Some(hue_track));
    changed |= slider(
        ui,
        &mut wheel.chroma,
        "Chroma",
        Spec::CHROMA,
        Some(chroma_track),
    );
    changed
        | slider(
            ui,
            &mut wheel.lightness,
            "Lightness",
            Spec::SIGNED,
            Some(lightness_track),
        )
}

/// Slider range and display/parse functions.
#[derive(Clone, Copy)]
struct Spec {
    range: (f32, f32),
    format: fn(f64) -> String,
    parse: fn(&str) -> Option<f64>,
}
impl Spec {
    const HUE: Self = Self {
        range: (0.0, 1.0),
        format: degrees_text,
        parse: parse_degrees,
    };
    const CHROMA: Self = Self {
        range: (0.0, 1.0),
        format: chroma_text,
        parse: parse_chroma,
    };
    const SIGNED: Self = Self {
        range: (-1.0, 1.0),
        format: percent,
        parse: parse_signed,
    };
}

type Track = fn(&mut egui::Ui, egui::Rect);

fn slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    label: &str,
    spec: Spec,
    track: Option<Track>,
) -> bool {
    let width = (ui.available_width() - 110.0).max(60.0);
    ui.spacing_mut().slider_width = width;
    let response = ui.add(
        egui::Slider::new(value, spec.range.0..=spec.range.1)
            .custom_formatter(|v, _| (spec.format)(v))
            .custom_parser(spec.parse)
            .text(label)
            .update_while_editing(false),
    );
    if let Some(track) = track {
        let left = response.rect.left();
        track(
            ui,
            egui::Rect::from_min_max(
                egui::pos2(left, response.rect.bottom() - TRACK),
                egui::pos2(left + width, response.rect.bottom()),
            ),
        );
    }
    // Double-click returns a slider to zero.
    let reset = response.double_clicked() && *value != 0.0;
    if reset {
        *value = 0.0;
    }
    response.changed() || reset
}
/// Rainbow colors at each stop boundary; static, so computed once.
fn rainbow() -> &'static [egui::Color32; RAINBOW_STOPS + 1] {
    static COLORS: OnceLock<[egui::Color32; RAINBOW_STOPS + 1]> = OnceLock::new();
    COLORS.get_or_init(|| {
        std::array::from_fn(|i| swatch(i as f32 / RAINBOW_STOPS as f32 * 360.0, 0.72, 1.0))
    })
}
fn hue_track(ui: &mut egui::Ui, rect: egui::Rect) {
    let step = rect.width() / RAINBOW_STOPS as f32;
    for (i, colors) in rainbow().windows(2).enumerate() {
        let part = egui::Rect::from_min_size(
            egui::pos2(rect.left() + step * i as f32, rect.top()),
            egui::vec2(step, rect.height()),
        );
        paint_gradient(ui, part, colors[0], colors[1]);
    }
}
fn chroma_track(ui: &mut egui::Ui, rect: egui::Rect) {
    paint_gradient(
        ui,
        rect,
        egui::Color32::from_gray(128),
        egui::Color32::from_rgb(210, 150, 110),
    );
}
fn lightness_track(ui: &mut egui::Ui, rect: egui::Rect) {
    paint_gradient(
        ui,
        rect,
        egui::Color32::from_gray(40),
        egui::Color32::from_gray(220),
    );
}

#[cfg(test)]
#[path = "../tests/unit/grading_ui_tests.rs"]
mod tests;
