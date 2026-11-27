//! Tone curve editor: master/red/green/blue tabs over a square plot of the
//! selected curve's control points, in encoded sRGB units.
use eframe::egui;
use filter_core::{
    curve::Spline,
    edit::{Curve, Curves},
};

/// Minimum x spacing kept between neighboring control points.
pub const GAP: f32 = 0.01;
/// Control points per curve accepted by the edit model.
pub const MAX_POINTS: usize = 1024;
/// Pointer distance, in points, that picks an existing control point.
const GRAB: f32 = 8.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    #[default]
    Master,
    Red,
    Green,
    Blue,
}
impl Channel {
    const ALL: [Self; 4] = [Self::Master, Self::Red, Self::Green, Self::Blue];
    fn label(self) -> &'static str {
        match self {
            Self::Master => "RGB",
            Self::Red => "Red",
            Self::Green => "Green",
            Self::Blue => "Blue",
        }
    }
    fn color(self) -> egui::Color32 {
        match self {
            Self::Master => egui::Color32::from_gray(230),
            Self::Red => egui::Color32::from_rgb(235, 90, 90),
            Self::Green => egui::Color32::from_rgb(90, 200, 110),
            Self::Blue => egui::Color32::from_rgb(100, 150, 245),
        }
    }
    pub fn curve(self, curves: &mut Curves) -> &mut Curve {
        match self {
            Self::Master => &mut curves.master,
            Self::Red => &mut curves.red,
            Self::Green => &mut curves.green,
            Self::Blue => &mut curves.blue,
        }
    }
}

/// The control point within `radius` (per axis, curve units) of `pos`, if any.
pub fn nearest(curve: &Curve, pos: [f32; 2], radius: [f32; 2]) -> Option<usize> {
    curve
        .0
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let d = [(p[0] - pos[0]) / radius[0], (p[1] - pos[1]) / radius[1]];
            (i, d[0] * d[0] + d[1] * d[1])
        })
        .filter(|(_, d)| *d <= 1.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}
/// Add an interior point, keeping x order and spacing; y is clamped to [0, 1].
pub fn insert(curve: &mut Curve, [x, y]: [f32; 2]) -> Option<usize> {
    let room = curve.0.len() < MAX_POINTS && x > GAP && x < 1.0 - GAP;
    if !room || curve.0.iter().any(|p| (p[0] - x).abs() < GAP) {
        return None;
    }
    let index = curve.0.partition_point(|p| p[0] < x);
    curve.0.insert(index, [x, y.clamp(0.0, 1.0)]);
    Some(index)
}
/// Move a point: endpoints keep their x; interior points stay between their
/// neighbors with [`GAP`] spacing. y is clamped to [0, 1].
pub fn move_point(curve: &mut Curve, index: usize, [x, y]: [f32; 2]) {
    let last = curve.0.len() - 1;
    let new_x = if index == 0 || index == last {
        curve.0[index][0]
    } else {
        let (lo, hi) = (curve.0[index - 1][0] + GAP, curve.0[index + 1][0] - GAP);
        // Imported points may sit closer than GAP; then x stays put.
        if lo <= hi {
            x.clamp(lo, hi)
        } else {
            curve.0[index][0]
        }
    };
    curve.0[index] = [new_x, y.clamp(0.0, 1.0)];
}
/// Remove an interior point; endpoints stay. Returns whether one was removed.
pub fn remove(curve: &mut Curve, index: usize) -> bool {
    let interior = index > 0 && index + 1 < curve.0.len();
    if interior {
        curve.0.remove(index);
    }
    interior
}

#[derive(Default)]
pub struct CurveUi {
    pub channel: Channel,
    dragging: Option<usize>,
    /// The plot's screen rect from the last frame, for headless tests.
    #[cfg(test)]
    pub plot: Option<egui::Rect>,
}
impl CurveUi {
    /// Returns whether the curves changed.
    pub fn show(&mut self, ui: &mut egui::Ui, curves: &mut Curves) -> bool {
        ui.horizontal(|ui| {
            for channel in Channel::ALL {
                if ui
                    .selectable_value(&mut self.channel, channel, channel.label())
                    .clicked()
                {
                    self.dragging = None;
                }
            }
        });
        let side = ui.available_width().clamp(120.0, 320.0);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click_and_drag());
        #[cfg(test)]
        {
            self.plot = Some(rect);
        }
        let response = response.on_hover_text(
            "Click to add a point · drag to move · double-click or right-click to remove",
        );
        let to_curve = |pos: egui::Pos2| {
            [
                (pos.x - rect.left()) / rect.width(),
                (rect.bottom() - pos.y) / rect.height(),
            ]
        };
        let radius = [GRAB / rect.width(), GRAB / rect.height()];
        let curve = self.channel.curve(curves);
        let changed = self.interact(&response, curve, to_curve, radius);
        let hovered = response
            .hover_pos()
            .and_then(|pos| nearest(curve, to_curve(pos), radius));
        paint(ui, rect, curve, self.channel, self.dragging.or(hovered));
        let focus = self.dragging.or(hovered).and_then(|i| curve.0.get(i));
        ui.label(match focus {
            Some([x, y]) => format!("Input {:.0} → output {:.0}", x * 255.0, y * 255.0),
            None => "Input → output (0–255)".to_owned(),
        });
        let identity = *curve == Curve::default();
        let reset = ui
            .add_enabled(!identity, egui::Button::new("Reset curve"))
            .clicked();
        if reset {
            *curve = Curve::default();
            self.dragging = None;
        }
        changed || reset
    }
    fn interact(
        &mut self,
        response: &egui::Response,
        curve: &mut Curve,
        to_curve: impl Fn(egui::Pos2) -> [f32; 2],
        radius: [f32; 2],
    ) -> bool {
        let press = response.interact_pointer_pos().map(&to_curve);
        let mut changed = false;
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(origin) = response.ctx.input(|i| i.pointer.press_origin())
        {
            let at = to_curve(origin);
            self.dragging = nearest(curve, at, radius).or_else(|| {
                let inserted = insert(curve, at);
                changed |= inserted.is_some();
                inserted
            });
        }
        if let (Some(index), Some(pos), true) = (self.dragging, press, response.dragged()) {
            // An undo during the drag may have replaced the curve.
            if index < curve.0.len() {
                let before = curve.0[index];
                move_point(curve, index, pos);
                changed |= curve.0[index] != before;
            }
        }
        if response.drag_stopped() {
            self.dragging = None;
        }
        if let Some(pos) = press {
            let near = nearest(curve, pos, radius);
            if response.double_clicked() || response.secondary_clicked() {
                changed |= near.is_some_and(|i| remove(curve, i));
            } else if response.clicked() && near.is_none() {
                changed |= insert(curve, pos).is_some();
            }
        }
        changed
    }
}

fn paint(ui: &egui::Ui, rect: egui::Rect, curve: &Curve, channel: Channel, active: Option<usize>) {
    let painter = ui.painter().with_clip_rect(rect.expand(6.0));
    let to_screen = |[x, y]: [f32; 2]| {
        egui::pos2(
            rect.left() + x * rect.width(),
            rect.bottom() - y * rect.height(),
        )
    };
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(28));
    let grid = egui::Stroke::new(1.0, egui::Color32::from_gray(52));
    for i in 1..4 {
        let t = i as f32 / 4.0;
        painter.vline(rect.left() + t * rect.width(), rect.y_range(), grid);
        painter.hline(rect.x_range(), rect.bottom() - t * rect.height(), grid);
    }
    painter.line_segment([to_screen([0.0, 0.0]), to_screen([1.0, 1.0])], grid);
    let spline = Spline::new(curve);
    let samples = (rect.width() as usize).clamp(64, 512);
    let line: Vec<egui::Pos2> = (0..=samples)
        .map(|i| {
            let x = i as f32 / samples as f32;
            to_screen([x, spline.eval(f64::from(x)) as f32])
        })
        .collect();
    let plot = ui.painter().with_clip_rect(rect);
    plot.add(egui::Shape::line(
        line,
        egui::Stroke::new(1.5, channel.color()),
    ));
    for (i, point) in curve.0.iter().enumerate() {
        let center = to_screen(*point);
        let selected = active == Some(i);
        painter.circle(
            center,
            if selected { 5.0 } else { 3.5 },
            if selected {
                channel.color()
            } else {
                egui::Color32::from_gray(28)
            },
            egui::Stroke::new(1.5, channel.color()),
        );
    }
}

#[cfg(test)]
#[path = "../tests/unit/curve_ui_tests.rs"]
mod tests;
