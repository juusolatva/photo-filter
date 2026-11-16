// Shared viewport/histogram output transform. Gamut compression and display
// dithering are pending; histogram measurements precede quantization/compositing.
fn output_linear_srgb(working: vec3<f32>) -> vec3<f32> {
    return vec3(dot(working,vec3(1.660491,-0.587641,-0.072850)),dot(working,vec3(-0.124550,1.132900,-0.008349)),dot(working,vec3(-0.018151,-0.100579,1.118730)));
}
fn encode_srgb(c: vec3<f32>) -> vec3<f32> {
    return select(1.055*pow(max(c,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*c,c<=vec3(0.0031308));
}
