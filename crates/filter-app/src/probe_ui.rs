use eframe::egui;
use filter_core::{ImageBuffer, cpu_eval::BasicAdjustments, probe::ProbeSample};
use filter_gpu::GpuProbe;

type Target = ([u32; 2], [[f32; 4]; 6]);
#[derive(Default)]
struct ProbeState {
    generation: u64,
    target: Option<Target>,
    submitted: Option<u64>,
    display: Option<ProbeSample>,
    error: Option<String>,
}
impl ProbeState {
    fn reset(&mut self) {
        self.generation += 1;
        self.target = None;
        self.submitted = None;
        self.display = None;
        self.error = None;
    }
    fn set_target(&mut self, target: Option<Target>) {
        if self.target != target {
            // Keep a current readback failure visible when the pointer moves to
            // Retry. Stale callbacks still cannot introduce an error.
            let error = self.error.take();
            self.reset();
            self.target = target;
            self.error = error;
        }
    }
    fn accept(&mut self, generation: u64, sample: Result<ProbeSample, String>) {
        if self.generation != generation || self.target.is_none() {
            return;
        }
        match sample {
            Ok(sample) => self.display = Some(sample),
            Err(error) => self.error = Some(error),
        }
    }
    fn retry(&mut self) {
        self.error = None;
        self.submitted = None;
    }
}

/// Map logical egui points through the same image rectangle used by the canvas.
/// This stays correct at any display scale; right/bottom edges are exclusive.
/// The probe measures the containing original pixel, not interpolated screen RGB.
pub fn source_pixel(
    pointer: Option<egui::Pos2>,
    viewport: egui::Rect,
    image: egui::Rect,
    size: [u32; 2],
) -> Option<[u32; 2]> {
    let p = pointer?;
    let visible = viewport.intersect(image);
    if !image.is_finite()
        || !image.is_positive()
        || size.contains(&0)
        || !p.x.is_finite()
        || !p.y.is_finite()
        || p.x < visible.left()
        || p.y < visible.top()
        || p.x >= visible.right()
        || p.y >= visible.bottom()
    {
        return None;
    }
    Some(std::array::from_fn(|i| {
        let fraction = (f64::from(p[i]) - f64::from(image.min[i])) / f64::from(image.size()[i]);
        ((fraction * f64::from(size[i])).floor() as u32).min(size[i] - 1)
    }))
}

pub struct ProbeUi {
    gpu: Result<GpuProbe, String>,
    state: ProbeState,
}
impl ProbeUi {
    pub const HEIGHT: f32 = 108.0;
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            gpu: GpuProbe::new(device).map_err(|e| format!("{e:#}")),
            state: ProbeState::default(),
        }
    }
    pub fn reset(&mut self) {
        self.state.reset();
    }
    pub fn is_pending(&self) -> bool {
        self.gpu.as_ref().is_ok_and(GpuProbe::is_pending)
    }
    pub fn refresh(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: Option<&ImageBuffer>,
        settings: Option<&BasicAdjustments>,
        coordinate: Option<[u32; 2]>,
    ) {
        let request = source
            .zip(settings)
            .zip(coordinate)
            .and_then(|((image, settings), xy)| {
                if xy[0] >= image.width() || xy[1] >= image.height() {
                    return None;
                }
                let pixel = image.pixels()
                    [xy[1] as usize * image.width() as usize + xy[0] as usize]
                    .map(|v| v.to_f32());
                Some((xy, settings, pixel))
            });
        // Invalidate before collecting: old cursor/image/edit samples and errors
        // cannot appear under the current coordinates, even for a single frame.
        self.state
            .set_target(request.map(|(xy, s, _)| (xy, *s.uniform_rows())));
        let Ok(gpu) = &mut self.gpu else {
            return;
        };
        for (generation, sample) in gpu.collect() {
            self.state
                .accept(generation, sample.map_err(|e| format!("{e:#}")));
        }
        if self.state.submitted != Some(self.state.generation)
            && self.state.error.is_none()
            && let Some((_, settings, pixel)) = request
            && gpu.try_submit(device, queue, pixel, settings, self.state.generation)
        {
            self.state.submitted = Some(self.state.generation);
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.label("Cursor color · source pixel").on_hover_text(
            "Original pixel under the cursor, after adjustments and sRGB clipping. RGB/hex use nearest 8-bit sRGB; Oklab describes the same output color. Alpha is separate. No checkerboard, flattening or dithering. Coordinates start at (0, 0). This is not the interpolated/minified screen color."
        );
        if let Some(error) = self.gpu.as_ref().err().or(self.state.error.as_ref()) {
            ui.colored_label(egui::Color32::LIGHT_RED, "Color probe unavailable")
                .on_hover_text(error);
            if self.gpu.is_ok() && ui.button("Retry color probe").clicked() {
                self.state.retry();
                ui.ctx().request_repaint();
            }
            return;
        }
        let Some((xy, _)) = self.state.target else {
            ui.label("Hover over the image");
            return;
        };
        ui.small(format!("Pixel ({}, {})", xy[0], xy[1]));
        match self.state.display {
            Some(sample @ ProbeSample::Color { oklab, alpha, .. }) => {
                let [r, g, b] = sample.rgb8().unwrap();
                ui.monospace(format!("RGB {r:3} {g:3} {b:3}  #{r:02X}{g:02X}{b:02X}"));
                ui.monospace(format!(
                    "Oklab {:.3} {:+.3} {:+.3}",
                    oklab[0], oklab[1], oklab[2]
                ));
                ui.small(format!("Alpha {:.1}% · sRGB", alpha * 100.0));
            }
            Some(ProbeSample::Transparent) => {
                ui.label("Transparent · alpha 0%");
            }
            Some(ProbeSample::Invalid) => {
                ui.label("Invalid pixel · nonfinite color");
            }
            None => {
                ui.label("Updating…");
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/probe_ui_tests.rs"]
mod tests;
