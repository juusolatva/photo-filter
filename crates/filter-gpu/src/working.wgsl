// Canonical v1 basic pointwise stages. Keep cpu_eval's reference in parity tests.
// Straight linear Rec.2020; no working-space clipping; alpha is unchanged.
struct BasicParams {
    wb0: vec4<f32>, wb1: vec4<f32>, wb2: vec4<f32>,
    tone0: vec4<f32>, tone1: vec4<f32>, color: vec4<f32>,
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
    // pow(negative, 1/3) is undefined in WGSL.
    let root = sign(lms) * pow(abs(lms), vec3(1.0/3.0));
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
fn apply_basic(pixel: vec4<f32>, p: BasicParams) -> vec4<f32> {
    let exposed = pixel.rgb * p.tone0.x;
    let rgb = vec3(dot(p.wb0.xyz, exposed), dot(p.wb1.xyz, exposed), dot(p.wb2.xyz, exposed));
    // Preserve the neutral/exposure-only path exactly, avoiding round-trip drift.
    if p.color.z == 0.0 { return vec4(rgb, pixel.a); }
    let lab = working_to_oklab(rgb);
    var l = lab.x;
    var x = clamp(l, 0.0, 1.0);
    l += p.tone0.y * 0.5 * x * (1.0-x);
    x = clamp(l, 0.0, 1.0);
    l += p.tone0.z * x * (1.0-x) * (2.0*x-1.0);
    x = clamp(l, 0.0, 1.0);
    l += p.tone1.x * 0.5 * x * (1.0-x)*(1.0-x)*(1.0-x);
    x = clamp(l, 0.0, 1.0);
    l += p.tone0.w * 0.5 * x*x*x * (1.0-x);
    l *= exp2(0.25 * p.tone1.y * smoothstep(0.5, 1.0, l));
    l += 0.05 * p.tone1.z * (1.0-smoothstep(0.0, 0.5, l));
    let chroma = length(lab.yz);
    let scale = (1.0+p.color.y) * (1.0+p.color.x*(1.0-clamp(chroma/0.4,0.0,1.0)));
    return vec4(oklab_to_working(vec3(l, lab.yz * scale)), pixel.a);
}
