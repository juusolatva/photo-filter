//! CPU reference for the basic pointwise WGSL stages, in straight linear Rec.2020:
//! calibration (with the folded channel mixer or monochrome), tone, tone curves, then
//! the HSL mixer, vibrance/saturation and color grading. Spatial filters and custom-stack execution are separate work.
use crate::{
    color::{oklab_to_rec2020, rec2020_to_oklab_perturbed, relative_white_balance, transform},
    curve::CurveLut,
    edit::{Calibration, Curves, EditState, ModelError, Tone},
    grading::{self, GRADE_ROWS, HSL_ROWS},
};
use rayon::prelude::*;
use std::sync::Arc;

/// Vec4 rows in the `BasicParams` uniform.
pub const BASIC_ROWS: usize = 16;
/// The `BasicParams` uniform: 16 vec4 rows, 256 bytes. See `working.wgsl`.
pub type BasicRows = [[f32; 4]; BASIC_ROWS];
/// First row of the HSL uniform data (hue, saturation, lightness).
const HSL_START: usize = 6;
/// First row of the grading data: shadows, midtones, highlights.
const GRADE_START: usize = 12;
/// Row holding the stage flags: x HSL active, y grading active, z any color stage
/// active, w grading pivot.
const FLAGS: usize = 15;

/// Validated immutable snapshot, shared by CPU probes, preview and tiled export.
#[derive(Clone, Debug)]
pub struct BasicAdjustments {
    pub(crate) rows: BasicRows,
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
        unsupported.color.hsl = Default::default();
        unsupported.color.grading = Default::default();
        unsupported.color.scalars.grading_balance = 0.0;
        unsupported.color.scalars.channel_mixer = EditState::default().color.scalars.channel_mixer;
        unsupported.color.scalars.monochrome = false;
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
        // Identity mixer with monochrome off keeps the white-balance rows untouched.
        let wb = grading::fold_mixer(&wb, &color.channel_mixer, color.monochrome).unwrap_or(wb);
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
        let hsl_active = grading::hsl_active(&edit.color.hsl);
        let grading_active = grading::grading_active(&edit.color.grading);
        let color_active =
            color.vibrance != 0.0 || color.saturation != 0.0 || hsl_active || grading_active;
        let curves = CurveLut::new(&edit.curves).map(Arc::new);
        let flag = |on: bool| if on { 1.0 } else { 0.0 };
        let mut rows = [[0.0; 4]; BASIC_ROWS];
        rows[..6].copy_from_slice(&[
            [wb[0][0], wb[0][1], wb[0][2], 0.0],
            [wb[1][0], wb[1][1], wb[1][2], 0.0],
            [wb[2][0], wb[2][1], wb[2][2], 0.0],
            [c.exposure.exp2(), t.brightness, t.contrast, t.highlights],
            [t.shadows, t.whites, t.blacks, flag(curves.is_some())],
            [
                color.vibrance,
                color.saturation,
                flag(tone_active || color_active),
                flag(tone_active),
            ],
        ]);
        rows[HSL_START..HSL_START + HSL_ROWS].copy_from_slice(&grading::hsl_rows(&edit.color.hsl));
        rows[GRADE_START..GRADE_START + GRADE_ROWS]
            .copy_from_slice(&grading::grade_rows(&edit.color.grading));
        rows[FLAGS] = [
            flag(hsl_active),
            flag(grading_active),
            flag(color_active),
            // Only read when grading is active; zero keeps neutral rows all-zero.
            if grading_active {
                grading::grade_pivot(color.grading_balance)
            } else {
                0.0
            },
        ];
        Ok(Self { rows, curves })
    }
    /// Exposure-only snapshot. Like EditState, accepts finite EV in [-5, 5].
    pub fn exposure(ev: f32) -> Result<Self, ModelError> {
        let mut edit = EditState::default();
        edit.calibration.exposure = ev;
        Self::new(&edit)
    }
    /// Sixteen vec4 rows, matching `BasicParams` in working.wgsl (256 bytes).
    /// Rows 0-2 are the channel mixer (or monochrome) folded into white balance.
    /// Row 4 lane 3 flags active curves; row 5 lanes 2/3 flag any perceptual
    /// (tone or color) and any tone control; rows 6-11 are the HSL mixer and
    /// rows 12-14 are the grading wheels and row 15 holds the stage flags.
    pub fn uniform_rows(&self) -> &BasicRows {
        &self.rows
    }
    /// Curve tables for the `ToneCurves` uniform, or `None` when the shader
    /// skips the curve stage (its uniform contents are then unused).
    pub fn curves(&self) -> Option<&Arc<CurveLut>> {
        self.curves.as_ref()
    }

    pub fn pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        self.pixel_perturbed(pixel, [0.0; 3])
    }
    /// `pixel` with every Oklab LMS cube root scaled by `1 + root_error[i]`;
    /// conditioning tests only.
    #[doc(hidden)]
    pub fn pixel_perturbed(&self, pixel: [f32; 4], root_error: [f32; 3]) -> [f32; 4] {
        let rec2020_to_oklab = |c| rec2020_to_oklab_perturbed(c, root_error);
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
                oklab_to_rec2020(self.color_stage([self.tone(l), a, b]))
            }
            Some(curves) => {
                let mut rgb = rgb;
                if tone_active != 0.0 {
                    let [l, a, b] = rec2020_to_oklab(rgb);
                    rgb = oklab_to_rec2020([self.tone(l), a, b]);
                }
                rgb = curves.apply(rgb);
                if self.rows[FLAGS][2] != 0.0 {
                    rgb = oklab_to_rec2020(self.color_stage(rec2020_to_oklab(rgb)));
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
    /// HSL, vibrance/saturation, then grading, on Oklab, matching `color_stage` in WGSL.
    fn color_stage(&self, mut lab: [f32; 3]) -> [f32; 3] {
        if self.rows[FLAGS][0] != 0.0 {
            lab = grading::hsl_apply(lab, &self.rows[HSL_START..HSL_START + HSL_ROWS]);
        }
        if self.rows[5][0] != 0.0 || self.rows[5][1] != 0.0 {
            lab = self.color(lab);
        }
        if self.rows[FLAGS][1] != 0.0 {
            let wheels = &self.rows[GRADE_START..GRADE_START + GRADE_ROWS];
            lab = grading::grade_apply(lab, wheels, self.rows[FLAGS][3]);
        }
        lab
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
