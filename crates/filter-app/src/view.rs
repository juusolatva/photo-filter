//! Canvas zoom, pan and 100% placement, separated from egui input plumbing so
//! the geometry can be tested headlessly.
use crate::preview::pixel_aligned;
use eframe::egui;

/// Canvas input for one frame, already filtered by hover/drag state.
#[derive(Clone, Copy, Debug, Default)]
pub struct ViewInput {
    /// The 100% control was clicked this frame.
    pub native_zoom: bool,
    /// Drag movement in points while the canvas is being dragged.
    pub drag: Option<egui::Vec2>,
    /// Smoothed vertical scroll while the canvas is hovered; zero otherwise.
    pub scroll: f32,
    /// Pointer over the canvas, used as the zoom anchor.
    pub pointer: Option<egui::Pos2>,
}

/// Where the image lands on the canvas this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Whole image rectangle in points, possibly extending past the viewport.
    pub image: egui::Rect,
    /// Part of `image` inside the viewport, pixel-aligned at 100%.
    pub visible: egui::Rect,
    /// Physical display pixels per source pixel; exactly 1 at 100%.
    pub physical_scale: f32,
}

/// Zoom relative to Fit, pan offset in points and the 100% mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    pub zoom: f32,
    pub native_pixels: bool,
    pub pan: egui::Vec2,
}
impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            native_pixels: false,
            pan: egui::Vec2::ZERO,
        }
    }
}
impl ViewState {
    /// Show the whole image centered, leaving 100% mode.
    pub fn fit(&mut self) {
        *self = Self::default();
    }

    /// Apply this frame's input and place a `source_size` image in `viewport`.
    /// Returns None, ignoring the input, when the viewport has no finite area.
    pub fn place(
        &mut self,
        viewport: egui::Rect,
        source_size: egui::Vec2,
        pixels_per_point: f32,
        input: ViewInput,
    ) -> Option<Placement> {
        let fit = (viewport.width() / source_size.x).min(viewport.height() / source_size.y);
        // Reject empty, NaN or infinite geometry, which would poison zoom and
        // pan. Inputs are checked directly because f32::min discards a NaN or
        // infinite operand (e.g. from a zero source dimension).
        let finite = viewport.is_finite()
            && source_size.is_finite()
            && source_size.min_elem() > 0.0
            && pixels_per_point.is_finite()
            && pixels_per_point > 0.0;
        if !finite || !fit.is_finite() || fit <= 0.0 {
            return None;
        }
        let physical_fit = fit * pixels_per_point;
        if input.native_zoom {
            self.native_pixels = true;
            self.pan = egui::Vec2::ZERO;
        }
        if self.native_pixels {
            // Recomputed every frame so 100% survives resizing and DPI changes.
            self.zoom = 1.0 / physical_fit;
        }
        if let Some(drag) = input.drag {
            self.pan += drag;
        }
        if input.scroll != 0.0 {
            self.zoom_around(viewport.center(), physical_fit, input.scroll, input.pointer);
        }
        let mut image = egui::Rect::from_center_size(
            viewport.center() + self.pan,
            source_size * fit * self.zoom,
        );
        let mut visible = image.intersect(viewport);
        if self.native_pixels {
            // Keep source texel boundaries on physical pixel boundaries at
            // 100%, including after panning, resizing or a DPI change.
            image = pixel_aligned(image, pixels_per_point);
            visible = pixel_aligned(image.intersect(viewport), pixels_per_point);
        }
        Some(Placement {
            image,
            visible,
            physical_scale: if self.native_pixels {
                1.0
            } else {
                physical_fit * self.zoom
            },
        })
    }

    /// Scroll zoom keeps the image point under the pointer fixed and leaves
    /// 100% mode. Limits allow 1% to 3200% physical scale, always including Fit.
    fn zoom_around(
        &mut self,
        center: egui::Pos2,
        physical_fit: f32,
        scroll: f32,
        pointer: Option<egui::Pos2>,
    ) {
        self.native_pixels = false;
        let old = self.zoom;
        self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(
            (0.01 / physical_fit).min(0.1),
            (32.0 / physical_fit).max(1.0),
        );
        if let Some(pointer) = pointer {
            let offset = pointer - center;
            self.pan = offset - (offset - self.pan) * (self.zoom / old);
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/view_tests.rs"]
mod tests;
