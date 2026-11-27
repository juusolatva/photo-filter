// Shared viewport/histogram output transform. Gamut compression and display
// dithering are pending; histogram measurements precede quantization/compositing.
fn output_linear_srgb(working: vec3<f32>) -> vec3<f32> {
    return vec3(dot(working,vec3(1.660491,-0.587641,-0.072850)),dot(working,vec3(-0.124550,1.132900,-0.008349)),dot(working,vec3(-0.018151,-0.100579,1.118730)));
}
fn encode_srgb(c: vec3<f32>) -> vec3<f32> {
    return select(1.055*pow(max(c,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*c,c<=vec3(0.0031308));
}
// True when any lane has an all-ones exponent (inf or NaN). Inspects bits
// rather than comparing, so adapter floating-point optimizations cannot hide NaN.
fn any_nonfinite3(v: vec3<f32>) -> bool {
    return any((bitcast<vec3<u32>>(v) & vec3(0x7f800000u)) == vec3(0x7f800000u));
}
// Exact-endpoint clipping tests of the histogram summary. Pass finite clamped
// output-transform values. The viewport overlay uses display-step margins instead.
fn shadow_clipped(rgb: vec3<f32>) -> bool { return any(rgb <= vec3(0.0)); }
fn highlight_clipped(rgb: vec3<f32>) -> bool { return any(rgb >= vec3(1.0)); }
