struct Params { basic: BasicParams, output: vec4<f32>, uv_rect: vec4<f32>, rect: vec4<f32> }
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> viewport: vec4<f32>;
@group(0) @binding(2) var<uniform> params: Params;
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
@fragment fn fs_main(in:VertexOut) -> @location(0) vec4<f32> {
    // Rasterizers quantize quad edges to a subpixel grid. Interpolating UVs
    // over each tile independently turns that quantization into color seams.
    // Derive every draw's sampling coordinates from the same physical viewport.
    let viewport_uv = (in.position.xy - viewport.xy) / viewport.zw;
    let local_uv = (viewport_uv - params.rect.xy) / params.rect.zw;
    let sample = sample_source(params.uv_rect.xy + local_uv * params.uv_rect.zw);
    let working = apply_basic(vec4(sample.rgb / max(sample.a,0.000001),sample.a),params.basic).rgb;
    let rgb = output_linear_srgb(working);
    let checker = select(0.18,0.28, (u32(in.position.x/12.0)+u32(in.position.y/12.0))%2u==0u);
    // Initial spike uses display clipping. Perceptual gamut compression is a later phase.
    let composite = mix(vec3(checker),clamp(rgb,vec3(0.0),vec3(1.0)),sample.a);
    return vec4(select(encode_srgb(composite),composite,params.output.x>0.5),1.0);
}
