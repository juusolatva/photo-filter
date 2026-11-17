// One original straight-alpha source pixel, independent of viewport minification.
@group(0) @binding(0) var<uniform> pixel: vec4<f32>;
@group(0) @binding(1) var<uniform> params: BasicParams;
struct ProbeResult { linear: vec4<f32>, status: vec4<u32> }
@group(0) @binding(2) var<storage, read_write> result: ProbeResult;

fn invalid4(v: vec4<f32>) -> bool {
    return any((bitcast<vec4<u32>>(v) & vec4(0x7f800000u)) == vec4(0x7f800000u));
}
@compute @workgroup_size(1)
fn main() {
    result.linear = vec4(0.0);
    result.status = vec4(0u);
    if pixel.a <= 0.0 { result.status.x = 1u; return; }
    if invalid4(pixel) { result.status.x = 2u; return; }
    let output = vec4(output_linear_srgb(apply_basic(pixel, params).rgb), pixel.a);
    if invalid4(output) { result.status.x = 2u; return; }
    result.linear = clamp(output, vec4(0.0), vec4(1.0));
}
