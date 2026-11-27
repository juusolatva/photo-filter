// Finishing 3D LUT stage. Mirrors filter-core::lut::CubeLut::tetra and
// cpu_eval::lut_stage expression for expression (no fma contraction in the CPU
// reference, so expect ~1 ulp differences). Only `textureLoad` reads the lattice:
// hardware filtering is never used. Includers declare, next to `curves`:
//   @group(0) @binding(4) var lut_tex: texture_3d<f32>;
//   @group(0) @binding(5) var<uniform> lutp: LutParams;
// Rows come from BasicAdjustments::lut_rows (all zero when the stage is skipped).
struct LutParams {
    dmin: vec4<f32>, // xyz domain min, w strength
    dmax: vec4<f32>, // xyz domain max, w active (0.0 or 1.0)
    inv: vec4<f32>,  // xyz 1 / (dmax - dmin), w f32(N - 1)
}
fn lut_at(c: vec3<u32>) -> vec3<f32> {
    return textureLoad(lut_tex, vec3<i32>(c), 0).rgb;
}
// `t` is finite and clamped to [0, 1]^3. Walks the tetrahedron of the sorted
// fractions from corner 000 to 111; ties break x before y before z.
fn lut_tetra(t: vec3<f32>, nm1: f32) -> vec3<f32> {
    let p = t * nm1;
    let i = min(vec3<u32>(p), vec3<u32>(u32(nm1) - 1u));
    let f = p - vec3<f32>(i);
    let c0 = lut_at(i);
    let c1 = lut_at(i + vec3<u32>(1u));
    var v: vec3<f32>;
    if f.x >= f.y {
        if f.y >= f.z {
            let a = lut_at(i + vec3<u32>(1u, 0u, 0u));
            let b = lut_at(i + vec3<u32>(1u, 1u, 0u));
            v = c0 + f.x * (a - c0) + f.y * (b - a) + f.z * (c1 - b);
        } else if f.x >= f.z {
            let a = lut_at(i + vec3<u32>(1u, 0u, 0u));
            let b = lut_at(i + vec3<u32>(1u, 0u, 1u));
            v = c0 + f.x * (a - c0) + f.z * (b - a) + f.y * (c1 - b);
        } else {
            let a = lut_at(i + vec3<u32>(0u, 0u, 1u));
            let b = lut_at(i + vec3<u32>(1u, 0u, 1u));
            v = c0 + f.z * (a - c0) + f.x * (b - a) + f.y * (c1 - b);
        }
    } else {
        if f.x >= f.z {
            let a = lut_at(i + vec3<u32>(0u, 1u, 0u));
            let b = lut_at(i + vec3<u32>(1u, 1u, 0u));
            v = c0 + f.y * (a - c0) + f.x * (b - a) + f.z * (c1 - b);
        } else if f.y >= f.z {
            let a = lut_at(i + vec3<u32>(0u, 1u, 0u));
            let b = lut_at(i + vec3<u32>(0u, 1u, 1u));
            v = c0 + f.y * (a - c0) + f.z * (b - a) + f.x * (c1 - b);
        } else {
            let a = lut_at(i + vec3<u32>(0u, 0u, 1u));
            let b = lut_at(i + vec3<u32>(0u, 1u, 1u));
            v = c0 + f.z * (a - c0) + f.y * (b - a) + f.x * (c1 - b);
        }
    }
    return v;
}
// Straight linear Rec.2020 in and out. The lookup input is clamped to the
// domain and the residual (e - v) passes through at unit slope, in one
// expression: e2 = e + s * (L - v).
fn lut_stage(rgb: vec3<f32>) -> vec3<f32> {
    if any_nonfinite3(rgb) { return rgb; }
    let lin = output_linear_srgb(rgb);
    let e = vec3(encode_mirrored(lin.x), encode_mirrored(lin.y), encode_mirrored(lin.z));
    let dmin = lutp.dmin.xyz;
    let v = min(max(e, dmin), lutp.dmax.xyz);
    let t = clamp((v - dmin) * lutp.inv.xyz, vec3(0.0), vec3(1.0));
    let looked_up = lut_tetra(t, lutp.inv.w);
    let e2 = e + lutp.dmin.w * (looked_up - v);
    return linear_srgb_to_working(vec3(decode_mirrored(e2.x), decode_mirrored(e2.y), decode_mirrored(e2.z)));
}
