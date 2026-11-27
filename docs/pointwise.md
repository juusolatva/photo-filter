# Basic pointwise renderer (process version 1)

The desktop app renders Exposure, relative Temperature/Tint, Brightness, Contrast,
Highlights/Shadows, Whites/Blacks, the channel mixer and monochrome, master/red/green/blue
tone curves, the 8-band HSL mixer, Vibrance and Saturation, then the three-wheel color grading,
and last an optional `.cube` look ([Cube LUT](#cube-lut-look)).
The mixer is folded into the white-balance matrix, so it runs right after exposure and
before tone; see [stage order](#channel-mixer-and-monochrome). The controls leave the
source untouched and feed both the proxy viewport and PNG/JPEG/WebP/TIFF export.
Export can retain full size or reduce by long edge/percentage; area filtering runs
after the captured pointwise adjustments and before output conversion.
The export captures the source and adjustments when its save dialog opens.
Snapshots are rebuilt only when edits change. Unsupported or invalid edits show
a diagnostic and disable export without panicking or showing a stale preview.
Reset clears the visible basic controls and preserves other edit sections.

`filter-gpu/src/working.wgsl` is the canonical pixel implementation, included by
both viewport and tile shaders. `filter-core::cpu_eval::BasicAdjustments` compiles
an immutable validated snapshot and provides `pixel` and rayon-backed `apply` for
CPU probes, future LUT baking and parity tests. GPU matrices and scalar uniforms
come from that same snapshot. The existing `ExposureRenderer` / `TiledExposure`
type names and exposure-only methods remain available; the new
`frame_region_adjusted` / `submit_adjusted` methods accept the full snapshot. Exposure-only methods now enforce the same
finite −5…+5 EV contract as `EditState`: tile submission returns an error outside
that range, while viewport helpers panic (documented in their API). Validate
untrusted input with `BasicAdjustments::exposure` before rendering.

```rust
use filter_core::{cpu_eval::BasicAdjustments, edit::EditState};

let mut edit = EditState::default();
edit.calibration.exposure = 0.5;
edit.calibration.temperature = 0.2;
edit.tone.contrast = 0.3;
edit.color.scalars.vibrance = 0.4;
let settings = BasicAdjustments::new(&edit)?;
let adjusted = settings.pixel([0.1, 0.2, 0.3, 0.5]); // straight linear Rec.2020
# Ok::<(), filter_core::edit::ModelError>(())
```

Exposure accepts −5…+5 EV. All other implemented controls, including every HSL band
value, grading lightness and grading balance, accept −1…+1, with zero neutral (grading hue
and chroma accept 0…1, channel mixer coefficients −2…+2); invalid values fail
`EditState::validate`, JSON deserialization, and transactional preset application. This
establishes the previously deferred v1 control ranges. Presets containing values outside
those ranges are rejected; there is no silent rescaling. Defringe must be finite and awaits
its engine definition.

Nondefault unsupported settings cause `BasicAdjustments::new` to return an error,
including clarity, detail, finishing, defringe and supported-schema custom nodes. Disabled, zero-opacity, and unknown
custom nodes are skipped. Unknown payloads remain in the original edit/preset.
This compiler does not mutate them. The current UI exposes implemented controls
and [selective preset file save/load](presets.md#desktop-workflow). Applying
unsupported selected effects leaves the current edit unchanged.

## Color and white balance

Working RGB is straight linear Rec.2020/D65. `color.rs` provides Rec.2020↔XYZ,
Rec.2020↔linear sRGB, Rec.2020↔linear Display P3, Oklab↔Rec.2020 and OkLCh↔Oklab
(hue in radians). Oklab uses a signed cube root, including on the GPU, so negative
RGB/LMS values remain defined. The CPU uses the correctly rounded `f32::cbrt`; the GPU
takes `pow(abs(x), 1/3)` and applies one Newton step (`r - (r - x/r²)/3`, guarded to
keep zero and denormal inputs finite), because the HSL hue weights and chroma gate
amplify the adapter-dependent `pow` error several times. No working RGB clamp is applied. The neutral and
exposure-only paths bypass Oklab to retain exact existing results.

Exposure multiplies RGB by `exp2(EV)`. White balance then adapts D65 to a relative
white using full CAT16 cone-response scaling, with target Y=1. For normalized
Temperature `T` and Tint `Q`, the target chromaticity is:

```
x = 0.3127 + 0.040 T + 0.010 Q
y = 0.3290 + 0.040 T - 0.025 Q
```

Positive Temperature warms; positive Tint adds magenta. These are creative
relative shifts from the already color-managed D65 image, not source-camera
Kelvin. The target is bounded and valid for the supported control rectangle.
Neutral white balance uses an exact identity matrix. The CPU builds the
Rec.2020→XYZ→CAT16→scaled CAT16→XYZ→Rec.2020 matrix once per snapshot; GPU and CPU
consume the same coefficients. The channel mixer is folded into this matrix (next
section).

Matrix references: [CSS Color 4 conversion code](https://www.w3.org/TR/css-color-4/#color-conversion-code),
[Oklab by its author](https://bottosson.github.io/posts/oklab/), and
[Colour's CAT16 matrix](https://github.com/colour-science/colour/blob/develop/colour/adaptation/datasets/cat.py).

### White balance picker

The eyedropper computes the Temperature and Tint that make a sampled color neutral.
It is CPU only. Exposure is a scalar and preserves chromaticity, and white balance is
the next stage, so the original straight-alpha linear Rec.2020 source pixels carry
exactly the chromaticity white balance adapts. The result is independent of the
current Temperature, Tint and exposure and of every later stage.

CAT16 adaptation is diagonal in CAT16 LMS with gains `LMS_target / LMS_D65`, where
`LMS_D65` is the LMS of the Rec.2020 white `[1, 1, 1]`. A sample with LMS `p` becomes
neutral (proportional to `LMS_D65`) exactly when `LMS_target` is proportional to
`LMS_D65² / p` elementwise. That target is converted to XYZ with the CAT16 inverse and
reduced to chromaticity `(x, y)`; the linear control mapping above is then inverted:

```
Q = ((x - 0.3127) - (y - 0.3290)) / 0.035
T = ((x - 0.3127) - 0.010 Q) / 0.040
```

Both controls are limited to [-1, 1]; `PickedBalance::clamped` reports a limited
result, which only partly neutralizes the sample. `filter_core::white_balance`
shares the CAT16 matrices and control constants with `relative_white_balance`.

The sample is the alpha-weighted straight-color mean of the 5 x 5 pixels centered on
the clicked source pixel, clipped at the image edges, skipping pixels with alpha <= 0
or any nonfinite component. Without any weight the result is `Transparent` (`Invalid`
if nonfinite pixels were skipped). The app maps the click to the source pixel with the
same `source_pixel` helper as the cursor probe. A sample is also rejected when its
luminance Y is below 0.001 (about ten stops under white, too dark to define a white;
`TooDark`), or its CAT16 response is nonfinite or not positive (`Invalid`).

Limits: the picker neutralizes the sample at the white balance stage only. Later
stages, such as per-channel RGB curves, can reintroduce a cast, and the tone stages
act on the already balanced color. It estimates nothing about the scene illuminant;
it trusts the chosen area to be neutral. Headless tests do not establish native
desktop pointer behavior.

## Perceptual controls

After calibration, convert RGB to Oklab. Tone controls change L in this sequence;
a/b remain unchanged. Each `x` below is `clamp(L, 0, 1)` recomputed after the
previous step. `S(a,b,L)` denotes smoothstep.

| Control | Update to L |
| --- | --- |
| Brightness B | `L += 0.5 B x (1-x)` |
| Contrast C | `L += C x (1-x) (2x-1)` |
| Shadows S | `L += 0.5 S x (1-x)^3` |
| Highlights H | `L += 0.5 H x^3 (1-x)` |
| Whites W | `L *= exp2(0.25 W S(0.5,1,L))` |
| Blacks K | `L += 0.05 K (1-S(0,0.5,L))` |

Brightness lifts or lowers midtones while fixing black and white; contrast is
an S curve around L=0.5. Shadow/highlight bases emphasize opposite parts of the
range. Whites changes the upper-range gain; Blacks changes the lower-range
pedestal. Extended L is retained rather than clipped; the first four corrections
are zero outside [0,1], while Whites/Blacks extend their endpoint behavior.
These formulas are application choices pinned by process version 1.

After the HSL mixer, color controls scale a and b by:

```
(1 + saturation) * (1 + vibrance * (1 - clamp(chroma / 0.4, 0, 1)))
```

Saturation −1 yields monochrome. Vibrance emphasizes lower-chroma colors; it is
not a skin-tone mask. Hue and L are preserved by this color step. Color grading
(below) then runs in the same round trip; convert back to Rec.2020 afterward. Alpha is copied unchanged, including zero-alpha pixels.

## HSL mixer

Eight bands (red, orange, yellow, green, aqua, blue, purple, magenta) each have a hue,
saturation and lightness value in −1…+1. The stage runs on Oklab `(L, a, b)` after the
tone curves and before vibrance/saturation, in the same Oklab round trip as
vibrance/saturation. Identity values (all 24 zero) skip it, so neutral, exposure-only and
vibrance/saturation-only output stay bit-identical.

Band centers are OkLCh hues measured from the repo's sRGB to Oklab conversion:

| Band | Reference | Hue (°) |
| --- | --- | --- |
| Red | #FF0000 | 29.234 |
| Orange | #FF8000 | 52.980 |
| Yellow | #FFFF00 | 109.769 |
| Green | #00FF00 | 142.495 |
| Aqua | #00FFFF | 194.769 |
| Blue | #0000FF | 264.052 |
| Purple | #8000FF | 293.940 |
| Magenta | #FF00FF | 328.363 |

The shader uses no `atan2`, `sin` or `cos`, avoiding adapter trigonometric precision
differences. Hue position uses the rational pseudo-angle `d(a, b)` in [0, 4), monotone in
the true angle (`b ≥ 0`: `a ≥ 0 ? b/(a+b) : 1 − a/(b−a)`; `b < 0`: `a < 0 ? 2 − b/(−a−b)
: 3 + a/(a−b)`). Segment breakpoints `D_k = fract((d(center_k) − d(center_0)) / 4) · 4`
with `D_8 = 4` are the constants `0, 0.21125777, 0.9055527, 1.2069472, 1.849796,
2.5468092, 2.9486322, 3.259941, 4` (`D0 = 0.35883468` is `d` of the red center); a test
recomputes them from the degrees above and parses the shader literals. For a pixel,
`hd = fract((d − d0) / 4) · 4` selects segment `k` (`D_k ≤ hd < D_{k+1}`), `t = (hd − D_k)
/ (D_{k+1} − D_k)`, `s = t²(3 − 2t)` and the next band `n = (k + 1) mod 8`. Only bands
`k` and `n` contribute, with weights `1 − s` and `s` (sum exactly one, flat at each
center, continuous across the magenta to red seam). Each parameter is
`mix(P[k], P[n], s)`, so a pure band color gets exactly its slider value.

Chroma `C = sqrt(a² + b²)`. A pixel with `a² + b² < 1e−12` is returned unchanged. A gate
`g = smoothstep(0.005, 0.04, C)` scales all three effects, so gray is untouched and
muted film colors (chroma 0.03–0.06) are affected gracefully. Weights use the input hue.
With interpolated `u`, `sat` and `le` (each times `g`):

| Effect | Update |
| --- | --- |
| Hue | the CPU stores `u_k = tan(15°) · hue_k`; `(a, b)` rotates by the Cayley half-angle form `cos = (1 − u²)/(1 + u²)`, `sin = 2u/(1 + u²)`, which is exactly unit-norm (full scale 2·atan(0.268) = ±30°, within 2% of linear). Positive increases OkLCh hue (red toward orange). |
| Saturation | `(a, b) *= 1 + g · mix(S_k, S_n, s)` (scale in [0, 2]; −1 removes the band, +1 doubles it) |
| Lightness | `L *= exp2(0.5 · g · mix(l_k, l_n, s))` (about ±1.5 stops of Y at full scale; multiplicative, so black stays black, negative L keeps its sign and HDR scales proportionally; a, b untouched) |

Lightness uses the pre-rotation chroma for its gate; hue is applied before saturation.
Results are not clipped here; the output transform clips. Adjacent bands with strongly
opposed hue shifts (for example red +100 and orange −100, whose centers are 23.75° apart)
compress the hues between them; the map stays continuous and finite. Multiplicative
lightness cannot lift black. All trigonometry and `tan` run once on the CPU when building
the uniform; GPU and CPU read the same f32 rows and use the same operation order.
HSL values are validated in −1…+1 and finite.

The desktop **Color mixer (HSL)** section has Hue, Saturation, Luminance and All tabs
over eight swatch-labeled bands, showing −100…+100 (the hue tab full scale is ±30°). Sliders
commit typed values on Enter or focus loss, a double-click zeroes a slider, and the
per-channel and global reset buttons are one undo step each. The tab is app state, not
part of the edit or undo history. If Monochrome is set the section
reminds that color mixing needs it off.

## Color grading

Three wheels (shadows, midtones, highlights) and a balance control act on Oklab after the
HSL mixer and vibrance/saturation, in the same round trip, so they cannot be undone by
Saturation −100: Saturation −100 plus a grade is a toned monochrome. The shader shares the
masks and offsets below with `filter-core::grading` (`grade_masks`, `grade_apply`).

Masks use the Oklab `L` that reaches the grading stage. The pivot is `c = 0.55 − 0.20 ×
balance` (0.35…0.75) and the blend width is fixed at 0.30:

```
ws = 1 − smoothstep(c − 0.30, c, L)
wh =     smoothstep(c, c + 0.30, L)
wm = 1 − ws − wh
```

The ramps are adjacent, so the weights are never negative, sum to one and `wm(c) = 1`;
`L ≤ 0` is all shadows and large `L` is all highlights. Positive balance lowers the pivot
so highlights grow. Each wheel has a hue in turns of OkLCh hue (0 and 1 are the same), a
chroma in 0…1 and a lightness in −1…+1. The CPU computes `(a_k, b_k) = 0.10 × chroma ×
(cos 2π hue, sin 2π hue)` and `dL_k = 0.09 × lightness` once; the shader only blends them:

```
L' = L + ws dL_0 + wm dL_1 + wh dL_2
(a', b') = (a, b) + smoothstep(0, 0.08, L) × (ws ab_0 + wm ab_1 + wh ab_2)
```

The tint is a constant additive cast: it applies to neutral pixels, is not scaled for HDR
values and fades out toward black so black and negative L are untinted. The lightness
lift is additive and not faded, so shadows lightness lifts blacks (a matte fade). Because
the ramps are adjacent only one slope term is active at a time and `dL'/dL ≥ 1 − 0.09 × 5 ×
2 = 0.10 > 0`; a test enforces this at every ±1 corner and balance, so do not raise 0.09 or
narrow the width without redoing it. Grading is active only if some wheel has chroma or
lightness nonzero: hue alone or balance alone is a no-op and keeps the stage skipped.
Values are validated (hue and chroma 0…1, lightness and balance −1…+1, all finite). Results
are not clipped here. These constants (0.10, 0.09, 0.30, 0.55, 0.08) are analysis-based and
should be tuned on real images before starter looks freeze them.

Starting points for a split tone: warm highlights and cool shadows, highlights hue 0.17
chroma 0.25, shadows hue 0.72 chroma 0.20, balance +0.1.

The desktop **Color grading** section has Shadows, Midtones and Highlights tabs, each with
Hue (shown 0–360° over a rainbow track), Chroma (0–100) and Lightness (−100…+100) sliders,
plus **Balance**. The tab is app state. Sliders commit typed values on Enter or focus loss,
a double-click zeroes a slider, and **Reset shadows/midtones/highlights** and **Reset
grading** (all wheels and balance) are one undo step each. A 2D wheel widget that writes the
same fields is planned.

## Channel mixer and monochrome

The mixer is a 3x3 matrix `M` (identity by default) whose rows are output channels:
`rgb' = M * rgb` in **linear sRGB primaries**, matching the R/G/B curves and photographers'
weights. Coefficients are −2…+2. It sits in the input stage, after exposure and white
balance and before tone, because film spectral sensitivity is a linear-light weighting that
the tone controls and curves then act on. A linear matrix is exact for negative, HDR and
out-of-gamut values, so this needs no shader change and no new uniforms.

Stage order is therefore: exposure gain, one folded 3x3 (mixer or monochrome after white
balance), Oklab tone and curves, then one Oklab round trip of HSL, vibrance/saturation and
grading. The white balance picker samples the source before this stage and is unaffected.

`filter_core::grading::fold_mixer` computes in f64 and narrows once. With `A` the repo's
linear sRGB to Rec.2020 matrix (`color::SRGB_TO_REC2020`) and `A⁻¹` its f64 inverse,
the mixer in the working space is `M₂₀₂₀ = A · M · A⁻¹`, and uniform rows 0–2 are
`M₂₀₂₀ · WB`. An identity mixer with monochrome off skips the fold entirely, so the rows are
exactly the white-balance rows and neutral output stays bit-identical.

Rows are not normalized: a row keeps neutral grays neutral only if its coefficients sum to
one. The UI shows each row sum (flagged when it is more than 1% from 100%) and offers
**Normalize rows**.

**Monochrome** outputs the luminance of the mixed color. With `Y` the linear sRGB luminance
weights, the gray weights are `w = Y · M`, converted to the working space as `w · A⁻¹` and
applied after white balance. All three uniform rows are that same vector, so linear-stage R,
G and B are bit-identical only through the linear stages; a tone or master-curve Oklab
round trip adds rounding noise of a few ulps, and per-channel R/G/B curves intentionally tone
the image, after which HSL acts on its real chroma.
`Y` is taken as `Y_2020 · A` (about 0.212639, 0.715169, 0.072192) rather than the
four-digit 0.2126/0.7152/0.0722, so with an identity mixer monochrome is exactly the Rec.2020
relative luminance `[0.2627002, 0.67799807, 0.059301715]` (a test pins it to 1e-6). The mixer
rows are never ignored or destroyed, so toggling monochrome loses nothing.

Black-and-white looks use a convention: set all three rows equal to weights that sum to one
and turn monochrome on, so that `Y · M` equals those weights (`EditState::apply_bw_response`).
Unchecking monochrome afterwards would leave a gray image, so the desktop UI also resets the
matrix to the identity in that same undo step when all three rows are equal; the schema has
no separate mono-weights field. `EditState::reset_mixer` restores the identity and turns
monochrome off; `reset_basic` preserves both. HSL has no effect on a pure mono image (its
chroma is zero and gated) and grading still tints it. For hue-selective B&W mixing use
Saturation −100 on a color image with HSL Luminance and monochrome off.

| Preset | R | G | B |
| --- | --- | --- | --- |
| Luminance (identity matrix on toggle) | 0.2126 | 0.7152 | 0.0722 |
| Panchromatic film | 0.30 | 0.58 | 0.12 |
| Orthochromatic | 0.08 | 0.60 | 0.32 |
| Yellow filter | 0.40 | 0.55 | 0.05 |
| Orange filter | 0.62 | 0.35 | 0.03 |
| Red filter | 0.80 | 0.20 | 0.00 |
| Green filter | 0.10 | 0.80 | 0.10 |

The desktop **Channel mixer** section has a Monochrome checkbox. Off, it shows the 3x3
percent grid (drag or type), row sums, **Normalize rows** (rows summing to zero, or whose
normalized coefficients would leave the range, are kept) and **Reset channel mixer**. On, it shows
three gray-weight sliders (the displayed `Y · M`; an edit writes all three rows equal) and
the preset buttons above. Each drag or button press is one undo step. Headless tests do not
establish native drag feel.

### Uniform layout

`BasicParams` is `cpu_eval::BASIC_ROWS = 16` vec4 rows, 256 bytes (it was six rows, 96
bytes). Row `i` is at byte offset `16 i`; rows 0–5 keep their positions. Tile uniforms are
the 256 bytes plus a 16-byte region (272 bytes); viewport frames are 256 plus 48 bytes.

| Row | Bytes | Content |
| --- | --- | --- |
| 0–2 | 0–47 | folded mixer × white balance matrix rows (`wb0..2`; plain white balance when the mixer is the identity and monochrome is off) |
| 3–4 | 48–79 | exposure gain and tone controls; row 4 lane 3 flags active curves |
| 5 | 80 | vibrance, saturation, `z` = tone or color active, `w` = tone active |
| 6–7 | 96–127 | HSL Cayley hue parameters, bands 0–3 then 4–7 |
| 8–9 | 128–159 | HSL saturation (raw −1…1) |
| 10–11 | 160–191 | HSL lightness × 0.5 |
| 12–14 | 192–239 | grading wheels (shadows, midtones, highlights): `(a offset, b offset, dL, 0)` |
| 15 | 240 | flags: `x` HSL active, `y` grading active, `z` any color stage (HSL, vibrance, saturation or grading) active, `w` grading pivot (written only while grading is active) |

With curves active the post-curve color stage runs when `flags.z` is set. Both the
no-curve and curve paths call one shared `color_stage` (HSL, then vibrance/saturation, then grading).

## Tone curves

Curves run after the tone controls and before the HSL mixer, vibrance/saturation and grading. Each curve is
a list of control points (see [the preset contract](presets.md)) interpolated by
a shape-preserving monotone cubic Hermite spline with PCHIP (Fritsch–Butland)
tangents: weighted harmonic means of adjacent secants, zero at local extrema and
plateaus, and limited three-point tangents at the ends. The spline passes through
every point and never overshoots between them, so monotone points give a
monotone curve.

`filter-core::curve::CurveLut` bakes all four curves to 1024 samples over
encoded [0, 1] (sample `i` at `i / 1023`) plus each spline's end tangents. Inside
[0, 1] a curve is the linear interpolation of its samples; below 0 and above 1 it
continues linearly from the end sample with the end tangent, preserving HDR
headroom and negative values. For each pixel:

```
linear sRGB = Rec.2020 → linear sRGB(working)
e           = mirrored sRGB encode(channel)        // sign(x) · sRGB(|x|)
e           = channel curve(master curve(e))       // red/green/blue per channel
working     = linear sRGB → Rec.2020(mirrored sRGB decode(e))
```

R/G/B curves therefore act on sRGB primaries, while values outside the sRGB gamut
or above 1 pass through the extended transfer. A curve whose points all lie on
y = x is the identity; when all four are, the stage and its round trip are
skipped, keeping the exact neutral and exposure-only results. Control-point y
values must be within −1…2 (encoded units). With curves active, tone and color
use separate Oklab round trips on either side of the curve stage.

The viewport, region tiles, export tiles, histogram and cursor probe bind the same
16,416-byte `ToneCurves` uniform layout. Viewport frames share one table per
renderer (`ExposureRenderer::set_curves`, called from the paint callback), so
per-tile uniforms stay small. Other paths write their own table before each
submission.

## Cube LUT (look)

An imported `.cube` 3D LUT is stored in `Finishing.lut` (`LutSlot`) with
`Finishing.lut_strength` (0 to 1) and runs after the color stage and before alpha. Stage
order: exposure/white balance/mixer, tone, curves, color stage (HSL, vibrance/saturation,
grading), **LUT**. Vignette and grain are not implemented; when they are, they come after the
LUT so grain is not graded by the look. The stage is skipped, bit-exactly, when no LUT is
present or the strength is 0. It is a wrapper around the existing adjustment body
(`apply_basic` over `apply_adjust` in WGSL, `pixel_perturbed` over `adjust_perturbed` in
the CPU reference), so the neutral and color-only early returns are unchanged.

```
if any_nonfinite(rgb) { return rgb }          // same exponent-bit test on CPU and GPU
lin = working → linear sRGB primaries
e   = mirrored sRGB encode(lin)               // per channel, as in the curve stage
v   = clamp(e, domain_min, domain_max)
t   = clamp((v - domain_min) * inv, 0, 1)     // inv = 1 / (domain_max - domain_min), f32
L   = tetrahedral(t)                          // absolute encoded sRGB, not rescaled
e2  = e + strength * (L - v)
out = linear sRGB → working(mirrored sRGB decode(e2))
```

`DOMAIN_MIN`/`DOMAIN_MAX` (or `LUT_3D_INPUT_RANGE`) map the **input** only; the file's
output values are absolute encoded sRGB. Input outside the domain is clamped for the lookup
and its residual `e - v` passes through at unit slope, so HDR, negative and wide-gamut
values stay finite and continuous at the domain edge instead of flattening to the lattice
edge. The slope is discontinuous at that edge. This is not gamut mapping; display and
export still clip to sRGB. Strength mixes in encoded sRGB, so 50% looks halfway on screen,
and an identity LUT stays the identity. Alpha is untouched.

**Assumption:** the file carries no color-space metadata, so the LUT is treated as expecting
sRGB-encoded input and output. LUTs made for log, Rec.709 gamma or video-range input will
look wrong, and the panel says so. `LUT_1D_*`, video-range keywords and unknown keywords are
rejected rather than ignored.

**Interpolation** is tetrahedral with explicit f32 arithmetic over `textureLoad` on a
`texture_3d<f32>` (`Rgba16Float`, x = R, y = G, z = B, R fastest in the file and the
texture); no sampler or hardware filtering is used. The cell index is
`min(trunc(t * (N - 1)), N - 2)` and the fraction orders the three axes with `>=` ties
resolved in one fixed order; the result is `((c0 + w1 d1) + w2 d2) + w3 d3` over the walk
from corner 000 to 111. The CPU reference (`CubeLut::tetra`) uses the same branches and
order and does not use `mul_add`; the shader compiler may contract to FMA, a difference of
about one ulp inside the existing tolerances, which are unchanged. On the gray axis the
result equals the diagonal lerp.

**Lattice precision.** Values are rounded to f16 once, at load, so the CPU and the GPU
interpolate the same lattice (6 bytes per node, 1.65 MB at N = 65). Interpolation between
nodes is f32 and continuous, but the lattice carries f16 quantization (a step of up to
2^-11 near 1), up to about 0.12 of an 8-bit step on a smooth gradient in a 16-bit export.
Output is not bit-accurate to the decimal values in the file. Lattice values must satisfy
`|v| <= 8`.

**GPU plumbing.** The LUT lives outside `BasicParams`, which stays 256 bytes. Each of the
viewport, tiled, histogram and probe shaders declares binding 4 (`lut_tex`) and binding 5
(`lutp: LutParams`, 48 bytes: domain minimum and strength, domain maximum and an active
flag, reciprocal width and `N - 1`). The shared `lut.wgsl` holds `lut_at`, `lut_tetra` and
`lut_stage`. The viewport, histogram and probe each own one fixed 65^3 texture and params
buffer, so their bind groups never go stale and a smaller LUT uploads into a sub-extent.
Tiled export creates one exactly sized texture per distinct LUT and a 1x1x1 dummy when none
is active. `set_lut` always writes the 48 bytes, and uploads the lattice only for a
different lattice, so a strength drag never re-uploads. The upload is a queue write; there is
no polling or readback. A size above the device 3D texture limit returns an error and the
viewport then renders without the LUT.

Neutrality audit: `BasicAdjustments::is_neutral()` is false for a LUT-only edit, and the
histogram/probe change detection compares the LUT together with the uniform rows
(`snapshot.rs`). Adding a stage that is neutral in `uniform_rows()` must update both.

## Output and verification

The viewport unpremultiplies its alpha-weighted source texture before these
operations, then composites clipped output RGB over its checkerboard. Both
hardware-sRGB and manually encoded targets are tested. Tiles use straight alpha
throughout. Output still uses the existing sRGB conversion and clipping; soft
gamut compression and display dithering remain pending. PNG8, TIFF8, WebP and JPEG export add seeded
8-bit RGB dithering; JPEG also adds linear-light alpha flattening. See
[the export contract](export.md). Half-float GPU texture storage also limits retained precision and range; this is not an HDR file exporter.

Ordinary tests cover color references, extended-range round trips, CAT16 white
chromaticity, exact neutral/exposure behavior, perceptual control behavior and
invalid/unsupported edits (defringe), channel mixer fold order, identity skip, monochrome
luminance and its bit-identical channels, neutral preservation and preset sums, as well as spline interpolation and shape preservation,
table sampling and extrapolation, stage order and the curve y range. HSL tests check band
centers against the repo's conversion, the shader and Rust constants, partition of unity
and continuity (including the red/magenta seam), exact band values at centers, uniform-band
global behavior, gate and gray bypass, hue shifts preserving chroma and lightness,
finite results for extreme and zero-chroma inputs, validation and the 256-byte uniform
layout. Grading tests check that the masks are non-negative and sum to one, the pivot
follows balance, the lightness map is strictly increasing with a slope of at least 0.1 at
every wheel-lightness corner, black and negative L stay untinted, hue-only and balance-only
edits are inert, and grading survives Saturation −100 and curves. Explicit software-adapter tests cover all 11 controls
at both endpoints, per-band, random-band and stacked HSL cases, grading per wheel at balance
−1, 0 and +1, all wheels together, and grading stacked with tone, curves, vibrance,
saturation and HSL, the channel mixer, monochrome and each B&W preset alone and stacked
with white balance, tone, curves, HSL, vibrance/saturation and grading, with hue sweeps across the
chroma gate, near-black, negative and HDR pixels, neutral and mixed settings on near-black, negative/HDR,
gradient and alpha pixels. They compare f32 shader results at
`2e-5 * (1 + abs(expected))`, half-float tiles at `0.001 * (1 + abs(expected))`,
and quantized f32 sRGB at a difference strictly below 2/255. Half-float tiles
must lie within one f16 step (plus the f32 tolerance) of the f16-rounded CPU
result. Their 8-bit display is not compared, because one f16 step can move a
near-black sRGB channel of a bright saturated pixel by 2/255. Curve cases cover an
S curve, per-channel curves, extended (negative/above-1) ends and curves combined
with tone and color. Monochrome with per-channel curves and HSL is in the parity set. The GPU cube root is
Newton-refined to within about one ulp of the CPU `cbrt`. A CPU conditioning test perturbs the
Oklab cube roots by a relative 1.2e-7 (one ulp for roots in [0.5, 1)) and requires the HSL
output to move by less than `2e-5 * (1 + |x|)`. HSL is steep enough that larger root errors
can exceed that bound with strongly opposed adjacent bands (about 1.9x at 2.5 ulp), so the
Newton step is required for parity. LUT tests cover the parser (grammar, limits, rejected keywords, a mutation sweep), lattice
exactness, all six tetrahedra and tie rules against a brute-force evaluation, the gray axis,
the out-of-domain policy, strength 0 and 1, non-finite pixels, and software-adapter parity of
the viewport f32 path and the f16 tile path (N = 2, 17 and 65, domains other than 0..1,
alpha, negative and above-1 pixels) plus histogram, probe, upload caching and a neutral edit
bit-exact with the no-LUT output. Tiled and untiled output
must match exactly. Separate viewport and PNG/JPEG/WebP/TIFF integration tests exercise
uniform layout, output transfer, alpha and the captured adjustment snapshot.
