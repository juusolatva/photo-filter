// Must match filter-core::histogram's flat readback layout (1040 u32 words).
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: BasicParams;
@group(0) @binding(2) var<storage, read_write> counters: array<atomic<u32>,1040>;
var<workgroup> local_counts: array<atomic<u32>,1040>;

fn accumulate(pixel: vec4<f32>) {
    if pixel.a <= 0.0 { atomicAdd(&local_counts[1033],1u); return; }
    // Inspect exponent bits rather than relying on floating-point comparisons
    // against NaN under adapter/compiler floating-point optimizations.
    if any((bitcast<vec4<u32>>(pixel) & vec4(0x7f800000u)) == vec4(0x7f800000u)) {
        atomicAdd(&local_counts[1034],1u); return;
    }
    let working = apply_basic(vec4(pixel.rgb / max(pixel.a,0.000001),pixel.a),params).rgb;
    let output = output_linear_srgb(working);
    if any((bitcast<vec3<u32>>(output) & vec3(0x7f800000u)) == vec3(0x7f800000u)) {
        atomicAdd(&local_counts[1034],1u); return;
    }
    let rgb = clamp(output,vec3(0.0),vec3(1.0));
    let luma = clamp(dot(rgb,vec3(0.2126,0.7152,0.0722)),0.0,1.0);
    let values = vec4(rgb,luma);
    let encoded = vec4(encode_srgb(rgb),encode_srgb(vec3(luma)).x);
    for(var c=0u;c<4u;c+=1u) {
        let bin = min(u32(encoded[c]*256.0),255u);
        atomicAdd(&local_counts[c*256u+bin],1u);
        if values[c] <= 0.0 { atomicAdd(&local_counts[1024u+c],1u); }
        if values[c] >= 1.0 { atomicAdd(&local_counts[1028u+c],1u); }
    }
    atomicAdd(&local_counts[1032],1u);
    if any(rgb <= vec3(0.0)) { atomicAdd(&local_counts[1035],1u); }
    if any(rgb >= vec3(1.0)) { atomicAdd(&local_counts[1036],1u); }
}

@compute @workgroup_size(16,16)
fn main(@builtin(global_invocation_id) id: vec3<u32>,
        @builtin(local_invocation_index) lane: u32) {
    for(var i=lane;i<1040u;i+=256u) { atomicStore(&local_counts[i],0u); }
    workgroupBarrier();
    let size = textureDimensions(source);
    if id.x<size.x && id.y<size.y {
        accumulate(textureLoad(source,vec2<i32>(id.xy),0));
    }
    // Every lane reaches both barriers, including out-of-bounds edge lanes.
    workgroupBarrier();
    for(var i=lane;i<1040u;i+=256u) {
        let count = atomicLoad(&local_counts[i]);
        if count>0u { atomicAdd(&counters[i],count); }
    }
}
