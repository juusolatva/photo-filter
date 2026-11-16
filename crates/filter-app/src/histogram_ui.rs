use eframe::egui;
use filter_core::{
    cpu_eval::BasicAdjustments,
    histogram::{BINS, Histogram},
};
use filter_gpu::{GpuHistogram, SourceTexture};

#[derive(Default)]
struct HistogramState {
    generation: u64,
    settings: Option<[[f32; 4]; 6]>,
    submitted: Option<u64>,
    display: Option<(u64, Histogram)>,
    error: Option<String>,
}
impl HistogramState {
    fn reset(&mut self) {
        self.generation += 1;
        self.settings = None;
        self.submitted = None;
        self.display = None;
        self.error = None;
    }
    fn set_settings(&mut self, settings: Option<[[f32; 4]; 6]>) {
        if self.settings != settings {
            self.generation += 1;
            self.settings = settings;
            self.submitted = None;
            self.error = None;
            if settings.is_none() {
                self.display = None;
            }
        }
    }
    fn accept(&mut self, generation: u64, result: Result<Histogram, String>) {
        if generation != self.generation || self.settings.is_none() {
            return;
        }
        match result {
            Ok(histogram) => {
                self.display = Some((generation, histogram));
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
    fn current(&self) -> bool {
        self.display
            .as_ref()
            .is_some_and(|(generation, _)| *generation == self.generation)
    }
    fn retry(&mut self) {
        self.error = None;
        self.submitted = None;
    }
}

pub struct HistogramUi {
    gpu: Result<GpuHistogram, String>,
    state: HistogramState,
    has_source: bool,
}
impl HistogramUi {
    pub const HEIGHT: f32 = 174.0;
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            gpu: GpuHistogram::new(device).map_err(|e| format!("{e:#}")),
            state: HistogramState::default(),
            has_source: false,
        }
    }
    pub fn reset(&mut self) {
        self.state.reset();
        if let Ok(gpu) = &mut self.gpu {
            gpu.clear_source();
        }
    }
    pub fn is_pending(&self) -> bool {
        self.gpu.as_ref().is_ok_and(GpuHistogram::is_pending)
    }

    pub fn refresh(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: Option<&SourceTexture>,
        settings: Option<&BasicAdjustments>,
    ) {
        self.has_source = source.is_some();
        let snapshot = source.and(settings).map(|s| *s.uniform_rows());
        // Determine the current generation before consuming any completed maps.
        self.state.set_settings(snapshot);
        let Ok(gpu) = &mut self.gpu else {
            return;
        };
        for (generation, result) in gpu.collect() {
            self.state
                .accept(generation, result.map_err(|e| format!("{e:#}")));
        }
        if self.state.submitted != Some(self.state.generation)
            && self.state.error.is_none()
            && let (Some(source), Some(settings)) = (source, settings)
        {
            match gpu.try_submit(device, queue, source, settings, self.state.generation) {
                Ok(true) => self.state.submitted = Some(self.state.generation),
                Ok(false) => {} // Both slots are occupied; coalesce to the latest settings.
                Err(error) => self.state.error = Some(format!("{error:#}")),
            }
        }
    }

    /// Paint into space reserved above the controls, after controls and presets
    /// have updated the desired snapshot. This avoids showing one-frame-old stats.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter();
        let text = ui.visuals().text_color();
        let font = egui::FontId::proportional(11.0);
        painter.text(
            rect.min,
            egui::Align2::LEFT_TOP,
            "Histogram",
            egui::FontId::proportional(16.0),
            text,
        );
        let colors = [
            egui::Color32::from_rgb(255, 100, 100),
            egui::Color32::from_rgb(100, 220, 130),
            egui::Color32::from_rgb(110, 160, 255),
            egui::Color32::LIGHT_GRAY,
        ];
        for (i, label) in ["R", "G", "B", "Y"].iter().enumerate() {
            painter.text(
                rect.min + egui::vec2(i as f32 * 28.0, 22.0),
                egui::Align2::LEFT_TOP,
                label,
                font.clone(),
                colors[i],
            );
        }
        let plot = egui::Rect::from_min_max(
            rect.min + egui::vec2(0.0, 40.0),
            egui::pos2(rect.max.x, rect.min.y + 130.0),
        );
        painter.rect_filled(plot, 3.0, egui::Color32::from_gray(20));
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let x = plot.left() + t * plot.width();
            painter.line_segment(
                [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                egui::Stroke::new(1.0, egui::Color32::from_gray(45)),
            );
        }
        let error = self.gpu.as_ref().err().or(self.state.error.as_ref());
        if let Some(error) = error {
            painter.text(
                plot.center() - egui::vec2(0.0, 22.0),
                egui::Align2::CENTER_CENTER,
                "Histogram unavailable",
                font.clone(),
                egui::Color32::LIGHT_RED,
            );
            ui.interact(
                plot,
                ui.scope_id().with("histogram_error"),
                egui::Sense::hover(),
            )
            .on_hover_text(error);
            if self.gpu.is_ok()
                && ui
                    .put(
                        egui::Rect::from_center_size(
                            plot.center() + egui::vec2(0.0, 6.0),
                            egui::vec2(110.0, 22.0),
                        ),
                        egui::Button::new("Retry histogram"),
                    )
                    .clicked()
            {
                self.state.retry();
                ui.ctx().request_repaint();
            }
            return;
        }
        let current = self.state.current();
        if let Some((_, histogram)) = &self.state.display {
            let peak = histogram
                .bins
                .iter()
                .flatten()
                .copied()
                .max()
                .unwrap_or(1)
                .max(1) as f32;
            for channel in [3, 0, 1, 2] {
                let points = histogram.bins[channel]
                    .iter()
                    .enumerate()
                    .map(|(i, count)| {
                        egui::pos2(
                            plot.left() + i as f32 / (BINS - 1) as f32 * plot.width(),
                            plot.bottom() - *count as f32 / peak * (plot.height() - 2.0),
                        )
                    })
                    .collect();
                painter.line(
                    points,
                    egui::Stroke::new(
                        1.0,
                        colors[channel].gamma_multiply(if current { 0.9 } else { 0.3 }),
                    ),
                );
            }
            if current {
                painter.text(
                    rect.min + egui::vec2(0.0, 136.0),
                    egui::Align2::LEFT_TOP,
                    format!(
                        "RGB clips: {} low · {} high",
                        histogram.low_pixels, histogram.high_pixels
                    ),
                    font.clone(),
                    text,
                );
                painter.text(
                    rect.min + egui::vec2(0.0, 153.0),
                    egui::Align2::LEFT_TOP,
                    format!("Preview · {} visible pixels", histogram.sampled),
                    font.clone(),
                    text,
                );
                if histogram.sampled == 0 {
                    painter.text(
                        plot.center(),
                        egui::Align2::CENTER_CENTER,
                        "No valid visible pixels",
                        font.clone(),
                        text,
                    );
                }
            }
        }
        if !current {
            painter.text(
                plot.center(),
                egui::Align2::CENTER_CENTER,
                if !self.has_source {
                    "Open an image"
                } else if self.state.settings.is_none() {
                    "Unsupported adjustments"
                } else {
                    "Updating…"
                },
                font.clone(),
                text,
            );
        }
        ui.interact(rect, ui.scope_id().with("histogram"), egui::Sense::hover()).on_hover_ui(|ui| {
            ui.label("Whole-image preview, independent of pan/zoom. RGB uses encoded sRGB; Y encodes linear Rec.709 luminance after output clipping. No checkerboard, alpha flattening or dithering.");
            ui.label("Fully transparent pixels are excluded; partial alpha counts once. RGB clipping counts pixels with any channel at black (≤0) or white (≥1), including exact endpoints. A pixel may count toward both.");
            if current && let Some((_, h)) = &self.state.display {
                ui.label(format!("Samples: {} · transparent: {} · invalid: {}", h.sampled, h.transparent, h.invalid));
                for (i, label) in ["R", "G", "B", "Y"].iter().enumerate() {
                    ui.label(format!("{label}: {} low · {} high", h.clipped_low[i], h.clipped_high[i]));
                }
            }
            if !current && self.state.display.is_some() { ui.label("Dimmed curves show the last completed edit while the current histogram updates."); }
        });
    }
}

#[cfg(test)]
#[path = "histogram_ui_tests.rs"]
mod tests;
