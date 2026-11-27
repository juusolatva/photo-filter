struct Params { basic: BasicParams, output: vec4<f32>, uv_rect: vec4<f32>, rect: vec4<f32> }
@group(0) @binding(0) var source: texture_2d<f32>;
// Physical framebuffer bounds (origin, size), and the compare split: fragments
// left of `compare.x` show the unedited source. `compare.y`/`compare.z` enable
// the shadow/highlight clipping overlay when above 0.5.
struct Viewport { bounds: vec4<f32>, compare: vec4<f32> }
@group(0) @binding(1) var<uniform> viewport: Viewport;
@group(0) @binding(2) var<uniform> params: Params;
// Shared by all frames of a renderer, like `viewport`.
@group(0) @binding(3) var<uniform> curves: ToneCurves;
struct VertexOut { @builtin(position) position: vec4<f32> }
@vertex fn vs_main(@builtin(vertex_index) i: u32) -> VertexOut {
    var uvs = array<vec2<f32>,6>(vec2(0.0,0.0),vec2(0.0,1.0),vec2(1.0,0.0),vec2(1.0,0.0),vec2(0.0,1.0),vec2(1.0,1.0));
    let uv = uvs[i];
    let p = params.rect.xy + uv * params.rect.zw;
    return VertexOut(vec4(p.x*2.0-1.0,1.0-p.y*2.0,0.0,1.0));
}
// Explicit f32 interpolation avoids adapter-dependent sampler weight precision.
// At native scale, tiny UV rounding around a texel center must not pull a
// visible amount of high-contrast color from the neighboring texel/tile border.
fn sample_source(uv: vec2<f32>) -> vec4<f32> {
    let size = textureDimensions(source);
    let p = uv * vec2<f32>(size) - vec2(0.5);
    let base = vec2<i32>(floor(p));
    let weight = fract(p);
    let last = vec2<i32>(size) - vec2(1);
    let a = textureLoad(source, clamp(base, vec2(0), last), 0);
    let b = textureLoad(source, clamp(base + vec2(1,0), vec2(0), last), 0);
    let c = textureLoad(source, clamp(base + vec2(0,1), vec2(0), last), 0);
    let d = textureLoad(source, clamp(base + vec2(1,1), vec2(0), last), 0);
    return mix(mix(a,b,weight.x),mix(c,d,weight.x),weight.y);
}
// Display-color overlay on the unclipped output of whatever this fragment shows.
// A channel is clipped when it would display as 8-bit sRGB 255 (highlights) or 0
// (shadows); fragments must be visible at 8 bits (alpha >= 0.5/255). These
// half-step margins keep f32 interpolation noise (~1e-6 of a neighbor texel)
// from flipping exact endpoint tests between tiled and untiled draws.
// Nonfinite output is not flagged, as the histogram counts it as invalid.
// Highlights win over shadows; other fragments keep the composite.
const OVERLAY_MIN_ALPHA: f32 = 0.5 / 255.0;
const DISPLAY_WHITE_LINEAR: f32 = 0.99554525; // sRGB decode of 254.5/255
const DISPLAY_BLACK_LINEAR: f32 = 0.5 / 255.0 / 12.92; // sRGB decode of 0.5/255
fn clipping_overlay(composite: vec3<f32>, rgb: vec3<f32>, alpha: f32) -> vec3<f32> {
    if alpha < OVERLAY_MIN_ALPHA || any_nonfinite3(rgb) { return composite; }
    if viewport.compare.z > 0.5 && any(rgb >= vec3(DISPLAY_WHITE_LINEAR)) { return vec3(1.0,0.0,0.0); }
    if viewport.compare.y > 0.5 && any(rgb < vec3(DISPLAY_BLACK_LINEAR)) { return vec3(0.0,0.0,1.0); }
    return composite;
}
@fragment fn fs_main(in:VertexOut) -> @location(0) vec4<f32> {
    // Rasterizers quantize quad edges to a subpixel grid. Interpolating UVs
    // over each tile independently turns that quantization into color seams.
    // Derive every draw's sampling coordinates from the same physical viewport.
    let viewport_uv = (in.position.xy - viewport.bounds.xy) / viewport.bounds.zw;
    let local_uv = (viewport_uv - params.rect.xy) / params.rect.zw;
    let sample = sample_source(params.uv_rect.xy + local_uv * params.uv_rect.zw);
    let straight = vec4(sample.rgb / max(sample.a,0.000001),sample.a);
    var working = straight.rgb;
    if (in.position.x >= viewport.compare.x) {
        working = apply_basic(straight,params.basic).rgb;
    }
    let rgb = output_linear_srgb(working);
    let checker = select(0.18,0.28, (u32(in.position.x/12.0)+u32(in.position.y/12.0))%2u==0u);
    // Initial spike uses display clipping. Perceptual gamut compression is a later phase.
    var composite = mix(vec3(checker),clamp(rgb,vec3(0.0),vec3(1.0)),sample.a);
    composite = clipping_overlay(composite,rgb,sample.a);
    return vec4(select(encode_srgb(composite),composite,params.output.x>0.5),1.0);
}
