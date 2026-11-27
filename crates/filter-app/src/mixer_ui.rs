//! Color mixer (HSL): per-band hue, saturation and luminance sliders over the
//! eight OkLCh bands, with a tab per channel or all three at once.
use eframe::egui;
use filter_core::{
    color::{linear_to_srgb, oklab_to_rec2020, rec2020_to_linear_srgb},
    edit::{Color, HslBand},
    grading::BAND_CENTER_DEGREES,
};
use std::sync::OnceLock;

pub const BAND_NAMES: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];
/// The hue slider's full-scale rotation, in OkLCh degrees (`2 atan(tan 15 deg)`).
pub const HUE_DEGREES: f32 = 30.0;
/// Thickness of the gradient strip under each slider.
const TRACK: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Hue,
    Saturation,
    Luminance,
}
impl Channel {
    pub const ALL: [Self; 3] = [Self::Hue, Self::Saturation, Self::Luminance];
    /// Position in [`Self::ALL`].
    fn index(self) -> usize {
        match self {
            Self::Hue => 0,
            Self::Saturation => 1,
            Self::Luminance => 2,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Luminance => "Luminance",
        }
    }
    pub fn slot(self, band: &mut HslBand) -> &mut f32 {
        match self {
            Self::Hue => &mut band.hue,
            Self::Saturation => &mut band.saturation,
            Self::Luminance => &mut band.lightness,
        }
    }
}

/// App state, not part of the edit or its undo history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Hue,
    Saturation,
    Luminance,
    All,
}
impl Tab {
    const TABS: [Self; 4] = [Self::Hue, Self::Saturation, Self::Luminance, Self::All];
    fn label(self) -> &'static str {
        match self {
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Luminance => "Luminance",
            Self::All => "All",
        }
    }
    pub fn channels(self) -> &'static [Channel] {
        match self {
            Self::Hue => &[Channel::Hue],
            Self::Saturation => &[Channel::Saturation],
            Self::Luminance => &[Channel::Luminance],
            Self::All => &Channel::ALL,
        }
    }
    fn channel(self) -> Option<Channel> {
        match self.channels() {
            [only] => Some(*only),
            _ => None,
        }
    }
}

/// Zero one channel of every band.
pub fn reset_channel(bands: &mut [HslBand; 8], channel: Channel) {
    bands.iter_mut().for_each(|band| *channel.slot(band) = 0.0);
}
/// Slider percent text: the -1..1 value shown as -100..+100.
pub fn percent(value: f64) -> String {
    format!("{:+.0}", value * 100.0)
}
/// Typed percent back to a -1..1 value, limited to the slider range.
pub fn parse_percent(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| (v / 100.0).clamp(-1.0, 1.0))
}

/// The largest chroma at lightness `l` and OkLCh hue `degrees` whose color stays
/// inside linear sRGB, found by bisection.
fn gamut_chroma(degrees: f32, l: f32) -> f32 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let inside = |chroma: f32| {
        rec2020_to_linear_srgb(oklab_to_rec2020([l, chroma * cos, chroma * sin]))
            .iter()
            .all(|v| (-0.001..=1.001).contains(v))
    };
    let (mut lo, mut hi) = (0.0, 0.5);
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        if inside(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}
/// Display color at an OkLCh hue, lightness and fraction of the gamut-limited chroma.
pub fn swatch(degrees: f32, l: f32, chroma_fraction: f32) -> egui::Color32 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let chroma = gamut_chroma(degrees, l) * chroma_fraction;
    let linear = rec2020_to_linear_srgb(oklab_to_rec2020([l, chroma * cos, chroma * sin]));
    let [r, g, b] = linear.map(|v| (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8);
    egui::Color32::from_rgb(r, g, b)
}
/// Band swatch: the gamut-scaled color at the band center. The table is built
/// once, since the swatches are static and each needs a gamut bisection.
pub fn band_color(band: usize) -> egui::Color32 {
    static COLORS: OnceLock<[egui::Color32; 8]> = OnceLock::new();
    COLORS.get_or_init(|| std::array::from_fn(|band| swatch(BAND_CENTER_DEGREES[band], 0.72, 1.0)))
        [band]
}
/// Colors at the minimum and maximum of a channel's slider track (cached).
pub fn track_colors(channel: Channel, band: usize) -> [egui::Color32; 2] {
    static COLORS: OnceLock<[[[egui::Color32; 2]; 8]; 3]> = OnceLock::new();
    COLORS.get_or_init(|| {
        Channel::ALL.map(|channel| std::array::from_fn(|band| compute_track_colors(channel, band)))
    })[channel.index()][band]
}
fn compute_track_colors(channel: Channel, band: usize) -> [egui::Color32; 2] {
    let degrees = BAND_CENTER_DEGREES[band];
    match channel {
        Channel::Hue => [
            swatch(degrees - HUE_DEGREES, 0.72, 1.0),
            swatch(degrees + HUE_DEGREES, 0.72, 1.0),
        ],
        Channel::Saturation => [swatch(degrees, 0.72, 0.0), swatch(degrees, 0.72, 1.0)],
        Channel::Luminance => [swatch(degrees, 0.35, 0.8), swatch(degrees, 0.92, 0.8)],
    }
}

#[derive(Default)]
pub struct MixerUi {
    pub tab: Tab,
}
impl MixerUi {
    /// Returns whether any band changed. Sliders apply typed values once on
    /// Enter or focus loss; drags and resets are single gestures for undo.
    pub fn show(&mut self, ui: &mut egui::Ui, color: &mut Color) -> bool {
        ui.horizontal_wrapped(|ui| {
            for tab in Tab::TABS {
                ui.selectable_value(&mut self.tab, tab, tab.label());
            }
        });
        if self.tab == Tab::Hue {
            ui.weak(format!("Full scale rotates ±{HUE_DEGREES:.0}°"));
        }
        if color.scalars.monochrome {
            ui.weak("Color mixing needs Monochrome off.");
        }
        let mut changed = false;
        for (index, band) in color.hsl.iter_mut().enumerate() {
            changed |= band_rows(ui, band, index, self.tab);
        }
        changed | self.reset_buttons(ui, &mut color.hsl)
    }
    fn reset_buttons(&self, ui: &mut egui::Ui, bands: &mut [HslBand; 8]) -> bool {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if let Some(channel) = self.tab.channel() {
                let active = bands.iter_mut().any(|band| *channel.slot(band) != 0.0);
                let label = format!("Reset {}", channel.label().to_lowercase());
                if ui.add_enabled(active, egui::Button::new(label)).clicked() {
                    reset_channel(bands, channel);
                    changed = true;
                }
            }
            let active = *bands != <[HslBand; 8]>::default();
            if ui
                .add_enabled(active, egui::Button::new("Reset HSL"))
                .clicked()
            {
                *bands = Default::default();
                changed = true;
            }
        });
        changed
    }
}

fn band_rows(ui: &mut egui::Ui, band: &mut HslBand, index: usize, tab: Tab) -> bool {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, band_color(index));
        ui.label(BAND_NAMES[index]);
    });
    let mut changed = false;
    for channel in tab.channels() {
        changed |= slider(ui, band, index, *channel, tab == Tab::All);
    }
    changed
}
fn slider(
    ui: &mut egui::Ui,
    band: &mut HslBand,
    index: usize,
    channel: Channel,
    label: bool,
) -> bool {
    let value = channel.slot(band);
    let width = (ui.available_width() - 70.0).max(60.0);
    ui.spacing_mut().slider_width = width;
    let mut slider = egui::Slider::new(value, -1.0..=1.0)
        .custom_formatter(|v, _| percent(v))
        .custom_parser(parse_percent)
        .update_while_editing(false);
    if label {
        slider = slider.text(channel.label());
    }
    let response = ui.add(slider);
    let [from, to] = track_colors(channel, index);
    let left = response.rect.left();
    let strip = egui::Rect::from_min_max(
        egui::pos2(left, response.rect.bottom() - TRACK),
        egui::pos2(left + width, response.rect.bottom()),
    );
    paint_gradient(ui, strip, from, to);
    // Double-click returns a slider to neutral.
    let reset = response.double_clicked() && *value != 0.0;
    if reset {
        *value = 0.0;
    }
    response.changed() || reset
}
pub(crate) fn paint_gradient(
    ui: &egui::Ui,
    rect: egui::Rect,
    from: egui::Color32,
    to: egui::Color32,
) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), from);
    mesh.colored_vertex(rect.right_top(), to);
    mesh.colored_vertex(rect.right_bottom(), to);
    mesh.colored_vertex(rect.left_bottom(), from);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
#[path = "../tests/unit/mixer_ui_tests.rs"]
mod tests;
