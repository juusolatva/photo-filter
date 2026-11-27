//! White-balance eyedropper: transient picker state, its status line and canvas
//! hint. The color math lives in `filter_core::white_balance`. Picker state is not
//! part of `EditState`; only the Temperature and Tint it sets are edits.
use eframe::egui;
use filter_core::{
    ImageBuffer,
    white_balance::{PickedBalance, pick_white_balance},
};

/// Toggles the picker; ignored while a text field has focus.
pub const TOGGLE_KEY: egui::Key = egui::Key::W;

enum Status {
    Applied(String),
    Failed(String),
}

#[derive(Default)]
pub struct Eyedropper {
    pub active: bool,
    status: Option<Status>,
}
impl Eyedropper {
    pub fn toggle(&mut self) {
        self.active = !self.active;
        self.status = None;
    }
    /// Leave picking without changing anything; the last status stays visible.
    pub fn cancel(&mut self) {
        self.active = false;
    }
    /// Forget everything, e.g. when another image replaces the one being sampled.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// W toggles (when `enabled`) and Esc cancels. Call only while no text
    /// field has focus.
    pub fn handle_keys(&mut self, ctx: &egui::Context, enabled: bool) {
        if enabled && crate::keys::consume_initial_press(ctx, TOGGLE_KEY) {
            self.toggle();
        }
        if self.active && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.cancel();
        }
    }
    /// Sample around `xy`. On success the picker deactivates and the balance is
    /// returned; an error keeps it active so another pixel can be tried.
    pub fn pick(&mut self, image: &ImageBuffer, xy: [u32; 2]) -> Option<PickedBalance> {
        match pick_white_balance(image, xy) {
            Ok(balance) => {
                self.active = false;
                self.status = Some(Status::Applied(applied_text(xy, balance)));
                Some(balance)
            }
            Err(error) => {
                self.status = Some(Status::Failed(format!(
                    "Pixel ({}, {}): {error}",
                    xy[0], xy[1]
                )));
                None
            }
        }
    }
    /// The toggle button and the status line, below it.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let button = egui::Button::selectable(self.active, "White balance picker (W)");
        if ui
            .add(button)
            .on_hover_text(
                "Click a neutral gray or white area of the image to set Temperature and Tint \
                 together · Esc cancels",
            )
            .clicked()
        {
            self.toggle();
        }
        match &self.status {
            Some(Status::Applied(text)) => {
                ui.small(text);
            }
            Some(Status::Failed(text)) => {
                ui.colored_label(egui::Color32::LIGHT_RED, text);
            }
            None => {}
        }
    }
    /// Crosshair over a pickable pixel and a hint over the canvas while active.
    pub fn paint_hint(&self, ui: &egui::Ui, viewport: egui::Rect, pickable: bool) {
        if pickable {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let painter = ui.painter().with_clip_rect(viewport);
        let text = "Click a neutral area to set white balance · Esc cancels";
        let font = egui::FontId::proportional(13.0);
        let galley = painter.layout_no_wrap(text.into(), font, egui::Color32::WHITE);
        let pos = egui::pos2(
            viewport.center().x - galley.size().x / 2.0,
            viewport.bottom() - galley.size().y - 12.0,
        );
        painter.rect_filled(
            egui::Rect::from_min_size(pos, galley.size()).expand(5.0),
            3.0,
            egui::Color32::from_black_alpha(190),
        );
        painter.galley(pos, galley, egui::Color32::WHITE);
    }
}

fn applied_text(xy: [u32; 2], balance: PickedBalance) -> String {
    let limited = if balance.clamped {
        " · limited to the slider range"
    } else {
        ""
    };
    format!(
        "Pixel ({}, {}): Temperature {:+.3}, Tint {:+.3}{limited}",
        xy[0], xy[1], balance.temperature, balance.tint
    )
}
