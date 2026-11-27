//! CPU reference for the basic pointwise WGSL stages, in straight linear Rec.2020:
//! calibration, tone, tone curves, then vibrance/saturation.
//! Spatial filters, grading, and custom-stack execution are separate work.
use crate::{
    color::{oklab_to_rec2020, rec2020_to_oklab, relative_white_balance, transform},
    curve::CurveLut,
    edit::{Calibration, Curves, EditState, ModelError, Tone},
};
use rayon::prelude::*;
use std::sync::Arc;

/// Validated immutable snapshot, shared by CPU probes, preview and tiled export.
#[derive(Clone, Debug)]
pub struct BasicAdjustments {
    pub(crate) rows: [[f32; 4]; 6],
    /// Baked tone curves; `None` when all are the identity.
    curves: Option<Arc<CurveLut>>,
}
impl Default for BasicAdjustments {
    fn default() -> Self {
        Self::new(&EditState::default()).expect("neutral edit")
    }
}
impl BasicAdjustments {
    /// Compile only the basic stages. Reject active effects not implemented by this
    /// renderer rather than silently dropping them. Opaque unknown nodes stay muted.
    pub fn new(edit: &EditState) -> Result<Self, ModelError> {
        edit.validate()?;
        let mut unsupported = edit.clone();
        unsupported.calibration = Calibration::default();
        unsupported.tone = Tone {
            clarity: edit.tone.clarity,
            ..Default::default()
        };
        unsupported.curves = Curves::default();
        unsupported.color.scalars.vibrance = 0.0;
        unsupported.color.scalars.saturation = 0.0;
        unsupported
            .custom_stack
            .retain(|n| n.effective_enabled() && n.opacity > 0.0);
        if unsupported != EditState::default() {
            return Err(ModelError(
                "edit contains effects outside the basic renderer".into(),
            ));
        }
        let c = &edit.calibration;
        let t = &edit.tone;
        let color = &edit.color.scalars;
        let wb = relative_white_balance(c.temperature, c.tint);
        let tone_active = [
            t.brightness,
            t.contrast,
            t.highlights,
            t.shadows,
            t.whites,
            t.blacks,
        ]
        .iter()
        .any(|v| *v != 0.0);
        let perceptual = tone_active || color.vibrance != 0.0 || color.saturation != 0.0;
        let curves = CurveLut::new(&edit.curves).map(Arc::new);
        Ok(Self {
            rows: [
                [wb[0][0], wb[0][1], wb[0][2], 0.0],
                [wb[1][0], wb[1][1], wb[1][2], 0.0],
                [wb[2][0], wb[2][1], wb[2][2], 0.0],
                [c.exposure.exp2(), t.brightness, t.contrast, t.highlights],
                [
                    t.shadows,
                    t.whites,
                    t.blacks,
                    if curves.is_some() { 1.0 } else { 0.0 },
                ],
                [
                    color.vibrance,
                    color.saturation,
                    if perceptual { 1.0 } else { 0.0 },
                    if tone_active { 1.0 } else { 0.0 },
                ],
            ],
            curves,
        })
    }
    /// Exposure-only snapshot. Like EditState, accepts finite EV in [-5, 5].
    pub fn exposure(ev: f32) -> Result<Self, ModelError> {
        let mut edit = EditState::default();
        edit.calibration.exposure = ev;
        Self::new(&edit)
    }
    /// Six vec4 rows, matching `BasicParams` in working.wgsl (96 bytes).
    /// Row 4 lane 3 flags active curves; row 5 lanes 2/3 flag any perceptual
    /// and any tone control.
    pub fn uniform_rows(&self) -> &[[f32; 4]; 6] {
        &self.rows
    }
    /// Curve tables for the `ToneCurves` uniform, or `None` when the shader
    /// skips the curve stage (its uniform contents are then unused).
    pub fn curves(&self) -> Option<&Arc<CurveLut>> {
        self.curves.as_ref()
    }

    pub fn pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        let gain = self.rows[3][0];
        let rgb = transform(
            std::array::from_fn(|i| [self.rows[i][0], self.rows[i][1], self.rows[i][2]]),
            [pixel[0] * gain, pixel[1] * gain, pixel[2] * gain],
        );
        let [_, _, perceptual, tone_active] = self.rows[5];
        let rgb = match &self.curves {
            // One Oklab round trip for tone and color, as before curves existed.
            None if perceptual == 0.0 => rgb,
            None => {
                let [l, a, b] = rec2020_to_oklab(rgb);
                oklab_to_rec2020(self.color([self.tone(l), a, b]))
            }
            Some(curves) => {
                let mut rgb = rgb;
                if tone_active != 0.0 {
                    let [l, a, b] = rec2020_to_oklab(rgb);
                    rgb = oklab_to_rec2020([self.tone(l), a, b]);
                }
                rgb = curves.apply(rgb);
                if self.rows[5][0] != 0.0 || self.rows[5][1] != 0.0 {
                    rgb = oklab_to_rec2020(self.color(rec2020_to_oklab(rgb)));
                }
                rgb
            }
        };
        [rgb[0], rgb[1], rgb[2], pixel[3]]
    }
    /// Tone controls applied to Oklab L, matching `tone_lightness` in WGSL.
    fn tone(&self, mut l: f32) -> f32 {
        let [_, brightness, contrast, highlights] = self.rows[3];
        let [shadows, whites, blacks, _] = self.rows[4];
        let mut x = l.clamp(0.0, 1.0);
        l += brightness * 0.5 * x * (1.0 - x);
        x = l.clamp(0.0, 1.0);
        l += contrast * x * (1.0 - x) * (2.0 * x - 1.0);
        x = l.clamp(0.0, 1.0);
        l += shadows * 0.5 * x * (1.0 - x).powi(3);
        x = l.clamp(0.0, 1.0);
        l += highlights * 0.5 * x.powi(3) * (1.0 - x);
        l *= (0.25 * whites * smoothstep(0.5, 1.0, l)).exp2();
        l + 0.05 * blacks * (1.0 - smoothstep(0.0, 0.5, l))
    }
    /// Vibrance/saturation applied to Oklab a/b, matching `color_scale` in WGSL.
    fn color(&self, [l, a, b]: [f32; 3]) -> [f32; 3] {
        let [vibrance, saturation, _, _] = self.rows[5];
        let chroma = a.hypot(b);
        let scale = (1.0 + saturation) * (1.0 + vibrance * (1.0 - (chroma / 0.4).clamp(0.0, 1.0)));
        [l, a * scale, b * scale]
    }
    /// Independent pixels are evaluated with rayon; alpha is copied unchanged.
    pub fn apply(&self, pixels: &mut [[f32; 4]]) {
        pixels.par_iter_mut().for_each(|p| *p = self.pixel(*p));
    }
}
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
