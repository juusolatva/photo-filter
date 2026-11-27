//! Keyboard helpers shared by the shortcut handlers.
use eframe::egui;

/// Consumes every press of `key` without modifiers, including OS key-repeat
/// events so they do not leak to widgets, and returns whether any of them was
/// an initial press. Toggles use this so holding a key fires them only once.
pub fn consume_initial_press(ctx: &egui::Context, key: egui::Key) -> bool {
    ctx.input_mut(|input| {
        let mut initial = false;
        input.events.retain(|event| match event {
            egui::Event::Key {
                key: pressed_key,
                modifiers,
                pressed: true,
                repeat,
                ..
            } if *pressed_key == key && modifiers.matches_logically(egui::Modifiers::NONE) => {
                initial |= !repeat;
                false
            }
            _ => true,
        });
        initial
    })
}
