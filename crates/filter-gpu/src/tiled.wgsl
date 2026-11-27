struct Params {
    basic: BasicParams,
    // Global pixel origin and full image dimensions, reserved for image-scale stages.
    region: vec4<u32>,
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var destination: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<uniform> curves: ToneCurves;
@group(0) @binding(4) var lut_tex: texture_3d<f32>;
@group(0) @binding(5) var<uniform> lutp: LutParams;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(source);
    if any(id.xy >= size) { return; }
    let pixel = textureLoad(source, vec2<i32>(id.xy), 0);
    textureStore(destination, vec2<i32>(id.xy), apply_basic(pixel, params.basic));
}
