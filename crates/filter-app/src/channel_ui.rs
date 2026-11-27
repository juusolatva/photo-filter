//! Channel mixer and monochrome. Off: a 3x3 grid of output-channel rows in linear
//! sRGB primaries with row sums and Normalize. On: three gray weights and the B&W
//! response presets. The mixer rows are never discarded by toggling monochrome.
use eframe::egui;
use filter_core::{
    color::{IDENTITY, Matrix3},
    edit::ColorScalars,
    grading::{BW_PRESETS, MIXER_LIMIT, mono_weights},
};

const CHANNELS: [&str; 3] = ["Red", "Green", "Blue"];
/// Row sums further than this from one are flagged: such a row shifts neutrals.
pub const SUM_TOLERANCE: f32 = 0.01;
/// Sums below this are treated as zero and left alone by Normalize.
const MIN_SUM: f32 = 1e-6;

/// Coefficient or weight as percent text, `0.5` as `50`.
pub fn percent_text(value: f64) -> String {
    format!("{:.0}", value * 100.0)
}
/// Typed percent back to a coefficient, limited to the mixer range.
pub fn parse_percent(text: &str) -> Option<f64> {
    let limit = f64::from(MIXER_LIMIT);
    text.trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| (v / 100.0).clamp(-limit, limit))
}
pub fn row_sum(row: &[f32; 3]) -> f32 {
    row.iter().sum()
}
/// Whether a row sum is far enough from 100% to warn about.
pub fn sum_is_off(sum: f32) -> bool {
    (sum - 1.0).abs() > SUM_TOLERANCE
}
/// The row scaled to sum to one, or `None` when that is a no-op (sum zero or
/// already one) or would push a coefficient outside the mixer range.
fn normalized(row: &[f32; 3]) -> Option<[f32; 3]> {
    let sum = row_sum(row);
    if sum.abs() < MIN_SUM || (sum - 1.0).abs() < MIN_SUM {
        return None;
    }
    let scaled = row.map(|v| v / sum);
    scaled
        .iter()
        .all(|v| v.abs() <= MIXER_LIMIT)
        .then_some(scaled)
}
/// Scale each row to sum to 100%. Rows that sum to zero, are already normal or
/// would leave the coefficient range are kept. Returns whether anything changed.
pub fn normalize_rows(mixer: &mut Matrix3) -> bool {
    let mut changed = false;
    for row in mixer.iter_mut() {
        if let Some(scaled) = normalized(row) {
            *row = scaled;
            changed = true;
        }
    }
    changed
}
/// Switch monochrome. Unchecking with all three rows equal (a B&W response, which
/// would otherwise leave a gray image) also resets the matrix to the identity, in
/// the same edit. Returns whether anything changed.
pub fn set_monochrome(color: &mut ColorScalars, on: bool) -> bool {
    if color.monochrome == on {
        return false;
    }
    color.monochrome = on;
    if !on
        && color.channel_mixer[0] == color.channel_mixer[1]
        && color.channel_mixer[1] == color.channel_mixer[2]
    {
        color.channel_mixer = IDENTITY;
    }
    true
}
/// Replace one gray weight and write the result to all three rows.
pub fn set_gray_weight(mixer: &mut Matrix3, channel: usize, value: f32) {
    let mut weights = mono_weights(mixer);
    weights[channel] = value;
    *mixer = [weights; 3];
}

/// Unit struct: the mixer has no app-side state beyond the edit.
#[derive(Default)]
pub struct ChannelUi;

impl ChannelUi {
    /// Returns whether the color changed. Drags and discrete actions end up as one
    /// undo step each through the app's gesture-based history.
    pub fn show(&mut self, ui: &mut egui::Ui, color: &mut ColorScalars) -> bool {
        let mut mono = color.monochrome;
        let mut changed = false;
        if ui.checkbox(&mut mono, "Monochrome").changed() {
            changed |= set_monochrome(color, mono);
        }
        changed |= if color.monochrome {
            gray_controls(ui, &mut color.channel_mixer)
        } else {
            grid_controls(ui, &mut color.channel_mixer)
        };
        changed | reset_button(ui, color)
    }
}

fn reset_button(ui: &mut egui::Ui, color: &mut ColorScalars) -> bool {
    let active = color.monochrome || color.channel_mixer != IDENTITY;
    if ui
        .add_enabled(active, egui::Button::new("Reset channel mixer"))
        .clicked()
    {
        color.channel_mixer = IDENTITY;
        color.monochrome = false;
        return true;
    }
    false
}

fn grid_controls(ui: &mut egui::Ui, mixer: &mut Matrix3) -> bool {
    ui.weak("Output rows in linear sRGB, percent of each input channel.");
    let mut changed = false;
    egui::Grid::new("channel_mixer_grid").show(ui, |ui| {
        ui.label("");
        for name in CHANNELS {
            ui.label(name);
        }
        ui.label("Sum");
        ui.end_row();
        for (row, name) in mixer.iter_mut().zip(CHANNELS) {
            ui.label(format!("{name} out"));
            for value in row.iter_mut() {
                changed |= coefficient(ui, value);
            }
            sum_label(ui, row_sum(row));
            ui.end_row();
        }
    });
    let can_normalize = mixer.iter().any(|row| normalized(row).is_some());
    if ui
        .add_enabled(can_normalize, egui::Button::new("Normalize rows"))
        .clicked()
    {
        changed |= normalize_rows(mixer);
    }
    changed
}
fn coefficient(ui: &mut egui::Ui, value: &mut f32) -> bool {
    let limit = MIXER_LIMIT;
    ui.add(
        egui::DragValue::new(value)
            .range(-limit..=limit)
            .speed(0.005)
            .custom_formatter(|v, _| percent_text(v))
            .custom_parser(parse_percent),
    )
    .changed()
}
fn sum_label(ui: &mut egui::Ui, sum: f32) {
    let text = format!("{}%", percent_text(f64::from(sum)));
    if sum_is_off(sum) {
        ui.colored_label(ui.visuals().warn_fg_color, text)
            .on_hover_text("This row does not sum to 100%, so neutral grays will shift.");
    } else {
        ui.label(text);
    }
}

fn gray_controls(ui: &mut egui::Ui, mixer: &mut Matrix3) -> bool {
    ui.weak("Gray = weighted sum of linear sRGB channels.");
    let mut changed = false;
    let weights = mono_weights(mixer);
    for (channel, name) in CHANNELS.iter().enumerate() {
        let mut value = weights[channel];
        let width = (ui.available_width() - 110.0).max(60.0);
        ui.spacing_mut().slider_width = width;
        let response = ui.add(
            egui::Slider::new(&mut value, -MIXER_LIMIT..=MIXER_LIMIT)
                .custom_formatter(|v, _| percent_text(v))
                .custom_parser(parse_percent)
                .text(*name)
                .update_while_editing(false),
        );
        if response.changed() {
            set_gray_weight(mixer, channel, value);
            changed = true;
        }
    }
    let total = row_sum(&weights);
    ui.horizontal(|ui| {
        ui.label("Sum");
        sum_label(ui, total);
    });
    ui.horizontal_wrapped(|ui| {
        for (name, preset) in BW_PRESETS {
            if ui.button(name).clicked() {
                *mixer = [preset; 3];
                changed = true;
            }
        }
    });
    changed
}

#[cfg(test)]
#[path = "../tests/unit/channel_ui_tests.rs"]
mod tests;
