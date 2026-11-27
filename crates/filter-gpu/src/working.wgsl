// Canonical v1 basic pointwise stages, then the Finishing LUT stage (lut.wgsl). Keep cpu_eval's reference in parity tests.
// Straight linear Rec.2020; no working-space clipping; alpha is unchanged.
// BasicParams is unchanged by the LUT: apply_basic wraps apply_adjust and reads the
// per-shader `lut_tex` / `lutp` globals (bindings 4 and 5) declared next to `curves`.
// tone1.w flags active curves; color.z/color.w flag any perceptual/tone control.
// flags.x/y/z flag an active HSL mixer / active grading / any active color stage and
// flags.w is the grading pivot. The 16 rows are 256 bytes, matching
// filter-core::cpu_eval::BasicRows. `grade` rows are per-wheel (a, b, dL, 0) for
// shadows, midtones and highlights. HSL rows hold bands 0-3 then 4-7 (red, orange, yellow,
// green, aqua, blue, purple, magenta): Cayley hue parameter tan(15 deg) * hue, raw
// saturation, and 0.5 * lightness.
struct BasicParams {
    wb0: vec4<f32>, wb1: vec4<f32>, wb2: vec4<f32>,
    tone0: vec4<f32>, tone1: vec4<f32>, color: vec4<f32>,
    hsl_u: array<vec4<f32>, 2>, hsl_sat: array<vec4<f32>, 2>, hsl_light: array<vec4<f32>, 2>,
    grade: array<vec4<f32>, 3>, flags: vec4<f32>,
}
// filter-core::curve::CurveLut rows: per-channel (master, red, green, blue)
// slopes below 0 and above 1, then 1024 samples over encoded [0, 1]. Every
// shader including this file binds a `curves: ToneCurves` uniform.
struct ToneCurves { lower: vec4<f32>, upper: vec4<f32>, samples: array<vec4<f32>, 1024> }
// Signed cube root. `pow` is only a few ulps accurate and its error is
// adapter-dependent, which the HSL hue weights and chroma gate amplify, so one
// Newton step (in the overflow-safe form r - (r - a/r^2)/3) brings the result to
// the correctly rounded `f32::cbrt` the CPU reference uses. Zero and values whose
// root is too small for r*r (denormals) keep the exact `pow` result.
fn cbrt_signed(x: vec3<f32>) -> vec3<f32> {
    let a = abs(x);
    let r = pow(a, vec3(1.0/3.0));
    let rr = r * r;
    let refined = r - (r - a / max(rr, vec3(1e-30))) / 3.0;
    // pow(negative, 1/3) is undefined in WGSL, so the sign is applied after.
    return sign(x) * select(r, refined, rr > vec3(1e-30));
}
fn working_to_oklab(c: vec3<f32>) -> vec3<f32> {
    let xyz = vec3(
        dot(c, vec3(0.636958, 0.1446169, 0.16888097)),
        dot(c, vec3(0.2627002, 0.67799807, 0.059301715)),
        dot(c, vec3(0.0, 0.028072692, 1.0609851)));
    let lms = vec3(
        dot(xyz, vec3(0.8190224, 0.36190626, -0.12887378)),
        dot(xyz, vec3(0.032983653, 0.92928684, 0.036144666)),
        dot(xyz, vec3(0.04817719, 0.26423952, 0.63354784)));
    let root = cbrt_signed(lms);
    return vec3(
        dot(root, vec3(0.21045427, 0.7936178, -0.004072043)),
        dot(root, vec3(1.9779985, -2.4285922, 0.4505937)),
        dot(root, vec3(0.025904043, 0.7827717, -0.80867577)));
}
fn oklab_to_working(c: vec3<f32>) -> vec3<f32> {
    let root = vec3(
        dot(c, vec3(1.0, 0.39633778, 0.21580376)),
        dot(c, vec3(1.0, -0.105561346, -0.06385417)),
        dot(c, vec3(1.0, -0.08948418, -1.2914855)));
    let lms = root*root*root;
    let xyz = vec3(
        dot(lms, vec3(1.2268798, -0.557815, 0.28139105)),
        dot(lms, vec3(-0.040575746, 1.1122868, -0.071711056)),
        dot(lms, vec3(-0.07637294, -0.42149332, 1.5869241)));
    return vec3(
        dot(xyz, vec3(1.7166512, -0.35567078, -0.2533663)),
        dot(xyz, vec3(-0.6666843, 1.6164812, 0.015768547)),
        dot(xyz, vec3(0.017639857, -0.042770613, 0.94210315)));
}
fn tone_lightness(lightness: f32, p: BasicParams) -> f32 {
    var l = lightness;
    var x = clamp(l, 0.0, 1.0);
    l += p.tone0.y * 0.5 * x * (1.0-x);
    x = clamp(l, 0.0, 1.0);
    l += p.tone0.z * x * (1.0-x) * (2.0*x-1.0);
    x = clamp(l, 0.0, 1.0);
    l += p.tone1.x * 0.5 * x * (1.0-x)*(1.0-x)*(1.0-x);
    x = clamp(l, 0.0, 1.0);
    l += p.tone0.w * 0.5 * x*x*x * (1.0-x);
    l *= exp2(0.25 * p.tone1.y * smoothstep(0.5, 1.0, l));
    return l + 0.05 * p.tone1.z * (1.0-smoothstep(0.0, 0.5, l));
}
fn color_scale(lab: vec3<f32>, p: BasicParams) -> vec3<f32> {
    let chroma = length(lab.yz);
    let scale = (1.0+p.color.y) * (1.0+p.color.x*(1.0-clamp(chroma/0.4,0.0,1.0)));
    return vec3(lab.x, lab.yz * scale);
}
// HSL mixer on Oklab, trig-free: bands are weighted by a rational pseudo-angle of
// (a, b), measured from the red band center; see filter-core::grading.
const HSL_D0: f32 = 0.35883468;
const HSL_BOUNDS = array<f32, 9>(
    0.0, 0.21125777, 0.9055527, 1.2069472, 1.849796,
    2.5468092, 2.9486322, 3.259941, 4.0);
struct HslSegment { k: u32, s: f32 }
fn pseudo_angle(a: f32, b: f32) -> f32 {
    if b >= 0.0 {
        if a >= 0.0 { return b / (a + b); }
        return 1.0 - a / (b - a);
    }
    if a < 0.0 { return 2.0 - b / (-a - b); }
    return 3.0 + a / (a - b);
}
fn hsl_segment(a: f32, b: f32) -> HslSegment {
    var bounds = HSL_BOUNDS;
    let x = (pseudo_angle(a, b) - HSL_D0) * 0.25;
    let hd = (x - floor(x)) * 4.0;
    var k = 0u;
    for (var j = 1u; j < 8u; j += 1u) {
        if hd >= bounds[j] { k += 1u; }
    }
    let t = (hd - bounds[k]) / (bounds[k + 1u] - bounds[k]);
    return HslSegment(k, t * t * (3.0 - 2.0 * t));
}
fn hsl_band_mix(rows: array<vec4<f32>, 2>, seg: HslSegment) -> f32 {
    var local = rows;
    let n = (seg.k + 1u) & 7u;
    return mix(local[seg.k >> 2u][seg.k & 3u], local[n >> 2u][n & 3u], seg.s);
}
fn hsl_stage(lab: vec3<f32>, p: BasicParams) -> vec3<f32> {
    let c2 = lab.y * lab.y + lab.z * lab.z;
    if c2 < 1e-12 { return lab; }
    let g = smoothstep(0.005, 0.04, sqrt(c2));
    let seg = hsl_segment(lab.y, lab.z);
    let u = g * hsl_band_mix(p.hsl_u, seg);
    let scale = 1.0 + g * hsl_band_mix(p.hsl_sat, seg);
    let l = lab.x * exp2(g * hsl_band_mix(p.hsl_light, seg));
    let denominator = 1.0 + u * u;
    let c = (1.0 - u * u) / denominator;
    let s = 2.0 * u / denominator;
    return vec3(l, (lab.y * c - lab.z * s) * scale, (lab.y * s + lab.z * c) * scale);
}
// Grading on Oklab: adjacent shadow/midtone/highlight masks around the pivot
// flags.w; additive tint (tapered near black) and lightness. See filter-core::grading.
fn grade_stage(lab: vec3<f32>, p: BasicParams) -> vec3<f32> {
    let c = p.flags.w;
    let ws = 1.0 - smoothstep(c - 0.30, c, lab.x);
    let wh = smoothstep(c, c + 0.30, lab.x);
    let wm = 1.0 - ws - wh;
    let q = smoothstep(0.0, 0.08, lab.x);
    let da = ws * p.grade[0].x + wm * p.grade[1].x + wh * p.grade[2].x;
    let db = ws * p.grade[0].y + wm * p.grade[1].y + wh * p.grade[2].y;
    let dl = ws * p.grade[0].z + wm * p.grade[1].z + wh * p.grade[2].z;
    return vec3(lab.x + dl, lab.y + q * da, lab.z + q * db);
}
// HSL, vibrance/saturation, then grading, on one Oklab round trip.
fn color_stage(lab: vec3<f32>, p: BasicParams) -> vec3<f32> {
    var out = lab;
    if p.flags.x != 0.0 { out = hsl_stage(out, p); }
    if p.color.x != 0.0 || p.color.y != 0.0 { out = color_scale(out, p); }
    if p.flags.y != 0.0 { out = grade_stage(out, p); }
    return out;
}
fn encode_mirrored(x: f32) -> f32 {
    let a = abs(x);
    return sign(x) * select(1.055*pow(a, 1.0/2.4)-0.055, 12.92*a, a <= 0.0031308);
}
fn decode_mirrored(e: f32) -> f32 {
    let a = abs(e);
    return sign(e) * select(pow((a+0.055)/1.055, 2.4), a/12.92, a <= 0.04045);
}
fn curve_lookup(e: f32, c: u32) -> f32 {
    if e < 0.0 { return curves.samples[0][c] + curves.lower[c] * e; }
    if e > 1.0 { return curves.samples[1023][c] + curves.upper[c] * (e - 1.0); }
    let t = e * 1023.0;
    // NaN yields some index here; the fraction stays NaN and propagates.
    let i = min(u32(t), 1022u);
    return mix(curves.samples[i][c], curves.samples[i+1u][c], t - f32(i));
}
fn linear_srgb_to_working(c: vec3<f32>) -> vec3<f32> {
    return vec3(
        dot(c, vec3(0.6274039, 0.32928303, 0.043313067)),
        dot(c, vec3(0.06909729, 0.9195404, 0.011362316)),
        dot(c, vec3(0.01639144, 0.088013306, 0.89559525)));
}
// Master then red/green/blue curves on mirrored sRGB values in sRGB primaries.
fn apply_curves(working: vec3<f32>) -> vec3<f32> {
    let linear = output_linear_srgb(working);
    var curved = vec3(0.0);
    for (var c = 0u; c < 3u; c += 1u) {
        curved[c] = decode_mirrored(curve_lookup(curve_lookup(encode_mirrored(linear[c]), 0u), c + 1u));
    }
    return linear_srgb_to_working(curved);
}
// Every stage before the LUT. BasicParams is unchanged by the LUT stage.
fn apply_adjust(pixel: vec4<f32>, p: BasicParams) -> vec4<f32> {
    let exposed = pixel.rgb * p.tone0.x;
    var rgb = vec3(dot(p.wb0.xyz, exposed), dot(p.wb1.xyz, exposed), dot(p.wb2.xyz, exposed));
    if p.tone1.w == 0.0 {
        // Preserve the neutral/exposure-only path exactly, avoiding round-trip drift.
        if p.color.z == 0.0 { return vec4(rgb, pixel.a); }
        let lab = working_to_oklab(rgb);
        return vec4(oklab_to_working(color_stage(vec3(tone_lightness(lab.x, p), lab.yz), p)), pixel.a);
    }
    // Tone curves sit between the tone and color stages.
    if p.color.w != 0.0 {
        let lab = working_to_oklab(rgb);
        rgb = oklab_to_working(vec3(tone_lightness(lab.x, p), lab.yz));
    }
    rgb = apply_curves(rgb);
    if p.flags.z != 0.0 {
        rgb = oklab_to_working(color_stage(working_to_oklab(rgb), p));
    }
    return vec4(rgb, pixel.a);
}
// Adjustments, then the Finishing LUT (lut.wgsl; its bindings 4 and 5 are declared
// by each includer). Inactive LUTs return the adjusted pixel bit-exactly.
fn apply_basic(pixel: vec4<f32>, p: BasicParams) -> vec4<f32> {
    let r = apply_adjust(pixel, p);
    if lutp.dmax.w == 0.0 { return r; }
    return vec4(lut_stage(r.rgb), r.a);
}
