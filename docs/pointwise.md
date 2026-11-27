# Basic pointwise renderer (process version 1)

The desktop app renders Exposure, relative Temperature/Tint, Brightness, Contrast,
Highlights/Shadows, Whites/Blacks, master/red/green/blue tone curves, Vibrance and
Saturation, in that order. The controls leave the
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

Exposure accepts −5…+5 EV. All other implemented controls accept −1…+1, with zero
neutral; invalid values fail `EditState::validate`, JSON deserialization, and
transactional preset application. This establishes the previously deferred v1
control ranges. Presets containing values outside those ranges are rejected;
there is no silent rescaling. Other modeled controls still require finite values
and await their engine definitions.

Nondefault unsupported settings cause `BasicAdjustments::new` to return an error,
including clarity, detail, finishing, HSL/grading, channel
mixer and supported-schema custom nodes. Disabled, zero-opacity, and unknown
custom nodes are skipped. Unknown payloads remain in the original edit/preset.
This compiler does not mutate them. The current UI exposes implemented controls
and [selective preset file save/load](presets.md#desktop-workflow). Applying
unsupported selected effects leaves the current edit unchanged.

## Color and white balance

Working RGB is straight linear Rec.2020/D65. `color.rs` provides Rec.2020↔XYZ,
Rec.2020↔linear sRGB, Rec.2020↔linear Display P3, Oklab↔Rec.2020 and OkLCh↔Oklab
(hue in radians). Oklab uses a signed cube root, including on the GPU, so negative
RGB/LMS values remain defined. No working RGB clamp is applied. The neutral and
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
consume the same coefficients.

Matrix references: [CSS Color 4 conversion code](https://www.w3.org/TR/css-color-4/#color-conversion-code),
[Oklab by its author](https://bottosson.github.io/posts/oklab/), and
[Colour's CAT16 matrix](https://github.com/colour-science/colour/blob/develop/colour/adaptation/datasets/cat.py).

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

Color controls scale a and b by:

```
(1 + saturation) * (1 + vibrance * (1 - clamp(chroma / 0.4, 0, 1)))
```

Saturation −1 yields monochrome. Vibrance emphasizes lower-chroma colors; it is
not a skin-tone mask. Hue and L are preserved by this color step. Convert back
to Rec.2020 afterward. Alpha is copied unchanged, including zero-alpha pixels.

## Tone curves

Curves run after the tone controls and before vibrance/saturation. Each curve is
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
invalid/unsupported edits, as well as spline interpolation and shape preservation,
table sampling and extrapolation, stage order and the curve y range. Explicit software-adapter tests cover all 11 controls
at both endpoints, neutral and mixed settings on near-black, negative/HDR,
gradient and alpha pixels. They compare f32 shader results at
`2e-5 * (1 + abs(expected))`, half-float tiles at `0.001 * (1 + abs(expected))`,
and quantized f32 sRGB at a difference strictly below 2/255. Half-float tiles
must lie within one f16 step (plus the f32 tolerance) of the f16-rounded CPU
result. Their 8-bit display is not compared, because one f16 step can move a
near-black sRGB channel of a bright saturated pixel by 2/255. Curve cases cover an
S curve, per-channel curves, extended (negative/above-1) ends and curves combined
with tone and color. Tiled and untiled output
must match exactly. Separate viewport and PNG/JPEG/WebP/TIFF integration tests exercise
uniform layout, output transfer, alpha and the captured adjustment snapshot.
